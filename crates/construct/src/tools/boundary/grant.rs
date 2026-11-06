//! Shared validation for absolute host grant deadlines on every boundary
//! observation (contract 1.1.0 §5.4).

use serde::Deserialize;
use serde_json::Value;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub fn optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(value) => Ok(Some(value)),
        _ => Err(serde::de::Error::custom("scope must be a string")),
    }
}

pub fn expires_at_ms<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Value::Number(number) = value else {
        return Err(serde::de::Error::custom("expires_at_ms must be an integer-valued JSON number"));
    };
    let deadline = number.as_u64().or_else(|| {
        let value = number.as_f64()?;
        (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= MAX_SAFE_INTEGER as f64)
            .then_some(value as u64)
    });
    match deadline {
        Some(deadline) if deadline <= MAX_SAFE_INTEGER => Ok(Some(deadline)),
        _ => Err(serde::de::Error::custom("expires_at_ms must be between 0 and 9007199254740991")),
    }
}

pub fn timed_has_deadline(scope: Option<&str>, expires_at_ms: Option<u64>) -> Result<(), String> {
    if scope == Some("timed") && expires_at_ms.is_none() {
        Err("timed boundary observation is missing expires_at_ms".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Record {
        #[serde(default, deserialize_with = "expires_at_ms")]
        deadline: Option<u64>,
    }

    fn parse(value: &str) -> Result<Option<u64>, serde_json::Error> {
        serde_json::from_str::<Record>(&format!(r#"{{"deadline":{value}}}"#)).map(|record| record.deadline)
    }

    #[test]
    fn deadline_accepts_schema_integers_and_rejects_every_other_json_shape() {
        assert_eq!(parse("0").unwrap(), Some(0));
        assert_eq!(parse("42.0").unwrap(), Some(42));
        assert_eq!(parse("9007199254740991").unwrap(), Some(9_007_199_254_740_991));
        for invalid in ["null", "true", r#""42""#, "1.5", "-1", "9007199254740992"] {
            assert!(parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn only_timed_scope_requires_a_deadline() {
        assert!(timed_has_deadline(Some("timed"), None).is_err());
        assert!(timed_has_deadline(Some("timed"), Some(0)).is_ok());
        assert!(timed_has_deadline(Some("future"), None).is_ok());
        assert!(timed_has_deadline(None, None).is_ok());
    }
}
