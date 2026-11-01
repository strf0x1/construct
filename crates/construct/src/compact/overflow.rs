//! Reading a provider's "prompt too long" rejection: whether it is one, and
//! the numbers it states. Providers word it differently, so this is a list of
//! phrases, and a number it gives replaces construct's own estimate.

/// Whether an error from the endpoint says the prompt was too long.
pub fn is_overflow(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "maximum context length",
        "context window",
        "prompt is too long",
        "too many tokens",
        "exceeds the context",
        "input is too long",
        "reduce the length",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// The prompt size a provider reported in an overflow error ("resulted in
/// 40123 tokens"), when it says.
pub fn overflow_tokens(err: &str) -> Option<u64> {
    number_after(err, "resulted in ")
}

/// The window a provider stated in an overflow error ("maximum context
/// length is 32768 tokens"), when it says.
pub fn overflow_limit(err: &str) -> Option<u64> {
    number_after(err, "context length is ")
}

fn number_after(s: &str, prefix: &str) -> Option<u64> {
    let (_, rest) = s.split_once(prefix)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
