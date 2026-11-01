//! Named driver keys encoded as terminal input, shared by scripted and
//! interactive driver callers.

use anyhow::{Result, bail};

pub(super) fn bytes(name: &str) -> Result<Vec<u8>> {
    Ok(match name {
        "enter" => b"\r".to_vec(),
        "esc" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "alt-pgup" => b"\x1b[5;3~".to_vec(),
        "alt-pgdn" => b"\x1b[6;3~".to_vec(),
        "pgup" => b"\x1b[5~".to_vec(),
        "pgdn" => b"\x1b[6~".to_vec(),
        "alt-enter" => b"\x1b\r".to_vec(),
        k if k.starts_with("ctrl-") && k.len() == 6 => {
            let c = k.as_bytes()[5].to_ascii_lowercase();
            vec![c - b'a' + 1]
        }
        other => bail!("unknown key {other}"),
    })
}
