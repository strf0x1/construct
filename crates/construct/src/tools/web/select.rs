//! Which backend answers a search. `[tools.web] search` is `auto` or one
//! backend's name. The design choice: `auto` is the keyed backends in a fixed
//! order (Exa, Brave, Tavily) and DuckDuckGo only when no key resolves at all,
//! so a query never goes to the keyless scrape because a keyed vendor was
//! down. A named backend is the only one tried.

use crate::secrets::{BRAVE_API_KEY, EXA_API_KEY, TAVILY_API_KEY};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Exa,
    Brave,
    Tavily,
    Ddg,
}

impl Backend {
    /// The order `auto` tries keyed backends in. A preference, not a ranking:
    /// the research found no measurable quality gap between them.
    const KEYED: [Backend; 3] = [Backend::Exa, Backend::Brave, Backend::Tavily];

    pub(super) fn label(self) -> &'static str {
        match self {
            Backend::Exa => "Exa",
            Backend::Brave => "Brave",
            Backend::Tavily => "Tavily",
            Backend::Ddg => "DuckDuckGo",
        }
    }

    /// The config name, and what `CONSTRUCT_TOOLS_WEB_SEARCH` takes.
    fn name(self) -> &'static str {
        match self {
            Backend::Exa => "exa",
            Backend::Brave => "brave",
            Backend::Tavily => "tavily",
            Backend::Ddg => "ddg",
        }
    }

    pub(super) fn secret(self) -> Option<&'static str> {
        match self {
            Backend::Exa => Some(EXA_API_KEY),
            Backend::Brave => Some(BRAVE_API_KEY),
            Backend::Tavily => Some(TAVILY_API_KEY),
            Backend::Ddg => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Auto,
    Only(Backend),
}

impl Choice {
    pub const NAMES: &'static str = "auto, exa, brave, tavily or ddg";

    pub fn parse(s: &str) -> Result<Self, String> {
        let all = [Backend::Exa, Backend::Brave, Backend::Tavily, Backend::Ddg];
        match s {
            "auto" => Ok(Choice::Auto),
            _ => all
                .into_iter()
                .find(|b| b.name() == s)
                .map(Choice::Only)
                .ok_or_else(|| format!("tools.web.search: unknown backend {s:?} (expected {})", Self::NAMES)),
        }
    }

    /// The backends to try, in order, each with its key (None for DuckDuckGo).
    /// Errors when a named backend has no key, with the command that fixes it.
    pub(super) fn plan(self, key: impl Fn(&str) -> Option<String>) -> Result<Vec<(Backend, Option<String>)>, String> {
        match self {
            Choice::Only(Backend::Ddg) => Ok(vec![(Backend::Ddg, None)]),
            Choice::Only(b) => {
                let secret = b.secret().unwrap_or_default();
                match key(secret) {
                    Some(k) => Ok(vec![(b, Some(k))]),
                    None => Err(format!(
                        "web_search is set to {} but {secret} is not set; run `construct secrets set {secret}`",
                        b.name()
                    )),
                }
            }
            Choice::Auto => {
                let keyed: Vec<_> =
                    Backend::KEYED.into_iter().filter_map(|b| Some((b, Some(key(b.secret()?)?)))).collect();
                Ok(if keyed.is_empty() { vec![(Backend::Ddg, None)] } else { keyed })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys<'a>(set: &'a [&'a str]) -> impl Fn(&str) -> Option<String> + 'a {
        move |n| set.contains(&n).then(|| format!("k-{n}"))
    }

    fn order(c: Choice, set: &[&str]) -> Vec<Backend> {
        c.plan(keys(set)).unwrap().into_iter().map(|(b, _)| b).collect()
    }

    #[test]
    fn auto_is_the_keyed_backends_in_order_then_nothing_else() {
        let all = [EXA_API_KEY, BRAVE_API_KEY, TAVILY_API_KEY];
        assert_eq!(order(Choice::Auto, &all), [Backend::Exa, Backend::Brave, Backend::Tavily]);
        assert_eq!(order(Choice::Auto, &[TAVILY_API_KEY, EXA_API_KEY]), [Backend::Exa, Backend::Tavily]);
    }

    #[test]
    fn auto_without_a_key_is_the_keyless_scrape() {
        assert_eq!(order(Choice::Auto, &[]), [Backend::Ddg]);
    }

    #[test]
    fn a_named_backend_is_the_only_one_tried() {
        let plan = Choice::Only(Backend::Tavily).plan(keys(&[EXA_API_KEY, TAVILY_API_KEY])).unwrap();
        assert_eq!(plan, [(Backend::Tavily, Some("k-TAVILY_API_KEY".into()))]);
        assert_eq!(order(Choice::Only(Backend::Ddg), &[EXA_API_KEY]), [Backend::Ddg]);
    }

    #[test]
    fn a_named_backend_without_its_key_says_how_to_set_it() {
        let err = Choice::Only(Backend::Exa).plan(keys(&[TAVILY_API_KEY])).unwrap_err();
        assert!(err.contains("EXA_API_KEY") && err.contains("construct secrets set EXA_API_KEY"), "{err}");
    }

    #[test]
    fn names_parse_and_unknown_ones_list_the_choices() {
        assert_eq!(Choice::parse("auto"), Ok(Choice::Auto));
        assert_eq!(Choice::parse("tavily"), Ok(Choice::Only(Backend::Tavily)));
        assert_eq!(Choice::parse("ddg"), Ok(Choice::Only(Backend::Ddg)));
        let err = Choice::parse("google").unwrap_err();
        assert!(err.contains("\"google\"") && err.contains(Choice::NAMES), "{err}");
    }
}
