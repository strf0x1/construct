//! The two optional narrowing arguments of `web_search`: how recent, and
//! which sites. Each backend maps them to its own native parameter; the
//! shared part is parsing the model's arguments and the date arithmetic.
//! The design choice: a bad value is dropped, never an error, because the
//! search is still worth running without the filter.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Recency {
    Day,
    Week,
    Month,
    Year,
}

impl Recency {
    pub(super) const NAMES: [&'static str; 4] = ["day", "week", "month", "year"];

    fn parse(s: &str) -> Option<Self> {
        match s {
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }

    /// Tavily's `time_range`.
    pub(super) fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// Brave's `freshness`.
    pub(super) fn brave(self) -> &'static str {
        ["pd", "pw", "pm", "py"][self as usize]
    }

    fn days(self) -> i64 {
        [1, 7, 30, 365][self as usize]
    }

    /// Exa's `startPublishedDate`: midnight UTC that many days before now.
    pub(super) fn since(self) -> String {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        since_iso(secs, self.days())
    }
}

fn since_iso(now_secs: i64, days_back: i64) -> String {
    let (y, m, d) = civil(now_secs.div_euclid(86_400) - days_back);
    format!("{y:04}-{m:02}-{d:02}T00:00:00.000Z")
}

/// (year, month, day) from days since 1970-01-01, Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Filter {
    pub recency: Option<Recency>,
    pub domains: Vec<String>,
}

impl Filter {
    pub(super) fn from_args(a: &Value) -> Self {
        let recency = a.get("recency").and_then(Value::as_str).and_then(Recency::parse);
        let domains = a
            .get("domains")
            .and_then(Value::as_array)
            .map(|d| d.iter().filter_map(Value::as_str).filter_map(host).collect())
            .unwrap_or_default();
        Self { recency, domains }
    }

    /// For a backend that can only take one site, as a `site:` operator in the
    /// query: the first domain, and a note when more were asked for.
    pub(super) fn site_query(&self, q: &str) -> (String, Option<String>) {
        match self.domains.split_first() {
            None => (q.to_string(), None),
            Some((first, rest)) => {
                let note =
                    (!rest.is_empty()).then(|| format!("only {first} was searched; this backend takes one domain"));
                (format!("site:{first} {q}"), note)
            }
        }
    }
}

/// A bare host from what the model wrote (`https://docs.rs/x` -> `docs.rs`).
/// Anything that could not be a hostname is dropped, so a domain can't carry
/// extra operators into the query.
fn host(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    let h = s.split(['/', '?', '#']).next()?.trim_end_matches('.').to_lowercase();
    let ok = !h.is_empty()
        && h.contains('.')
        && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*'));
    ok.then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_are_midnight_utc_n_days_back() {
        // 2026-10-28 12:00:00 UTC
        let now = 1_793_188_800;
        assert_eq!(since_iso(now, 0), "2026-10-28T00:00:00.000Z");
        assert_eq!(since_iso(now, 7), "2026-10-21T00:00:00.000Z");
        assert_eq!(since_iso(now, 30), "2026-09-28T00:00:00.000Z");
        // Across a leap day and a year boundary.
        assert_eq!(since_iso(1_709_208_000, 1), "2024-02-28T00:00:00.000Z");
        assert_eq!(since_iso(1_767_312_000, 3), "2025-12-30T00:00:00.000Z");
    }

    #[test]
    fn every_recency_has_each_backends_name() {
        for (n, (tavily, brave)) in
            Recency::NAMES.iter().zip([("day", "pd"), ("week", "pw"), ("month", "pm"), ("year", "py")])
        {
            let r = Recency::parse(n).unwrap();
            assert_eq!((r.name(), r.brave()), (tavily, brave));
        }
    }

    #[test]
    fn arguments_are_forgiving() {
        let f = Filter::from_args(
            &json!({ "recency": "week", "domains": ["https://Docs.rs/tokio", "doc.rust-lang.org/std"] }),
        );
        assert_eq!(f.recency, Some(Recency::Week));
        assert_eq!(f.domains, ["docs.rs", "doc.rust-lang.org"]);
        let f = Filter::from_args(
            &json!({ "recency": "fortnight", "domains": ["a b.com", "x\" OR site:evil.com", "localhost", 3] }),
        );
        assert_eq!(f, Filter::default());
        assert_eq!(Filter::from_args(&json!({})), Filter::default());
    }

    #[test]
    fn a_one_site_backend_gets_the_first_domain_and_a_note() {
        let f = Filter { recency: None, domains: vec!["docs.rs".into(), "crates.io".into()] };
        let (q, note) = f.site_query("tokio spawn");
        assert_eq!(q, "site:docs.rs tokio spawn");
        assert!(note.unwrap().contains("docs.rs"));
        assert_eq!(Filter::default().site_query("x"), ("x".into(), None));
    }
}
