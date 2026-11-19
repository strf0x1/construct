//! Clipboard through the terminal: OSC 52, which works over SSH and needs no
//! mouse capture. The terminal decides whether to honour it; there is no reply.

use std::io::Write;

/// Terminals commonly cap an OSC 52 payload near 100 kB of base64.
const MAX_BYTES: usize = 70_000;

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The escape sequence that sets the system clipboard to `text`, and whether
/// the text had to be cut to fit.
pub(super) fn osc52(text: &str) -> (String, bool) {
    let cut = crate::text::head(text, MAX_BYTES);
    (format!("\x1b]52;c;{}\x07", base64(cut.as_bytes())), cut.len() < text.len())
}

/// Write the sequence to the terminal. Returns whether the text was cut.
pub(super) fn copy(text: &str) -> bool {
    let (seq, cut) = osc52(text);
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let (seq, cut) = osc52(&"é".repeat(MAX_BYTES));
        assert!(cut && seq.starts_with("\x1b]52;c;") && seq.ends_with('\x07'));
    }
}
