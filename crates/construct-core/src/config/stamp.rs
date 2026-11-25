//! The stamp (#363): settings appended to a built binary by `construct build`,
//! so one file dropped on a machine without a compiler dials its hub with no
//! setup. The file is `[binary][payload][sha256 (32 B)][len (u32 LE)]["CNSTPEER"]`;
//! the payload is TOML holding the peer's dial settings (`[peer]`) and a
//! config layer (`[config]`) that sits between the baked layer and the file
//! (ADR 0050, 0073). No secrets and no join token are ever stamped.
//!
//! The design choice: a trailer that is present but wrong fails closed. A
//! bad length, a checksum mismatch, a payload that does not parse, or a
//! trailer cut short is an error naming the binary, never a silent run on
//! unstamped defaults, because those defaults would drop a stamped sandbox.
//! Only a binary with no trailer at all runs unstamped.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The last eight bytes of a stamped binary.
pub const MAGIC: [u8; 8] = *b"CNSTPEER";
/// sha256, then the payload length, then the magic.
const TRAILER: usize = 32 + 4 + MAGIC.len();
/// The largest payload a reader accepts; a stamp is a few hundred bytes.
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// The target triple this binary was built for.
pub const BUILD_TARGET: &str = env!("CONSTRUCT_BUILD_TARGET");
/// The payload format this build writes and reads.
pub const VERSION: u32 = 1;
/// The fewest leading bytes of the magic that, ending a file, mark a
/// trailer cut short rather than an unstamped binary.
const CUT_MAGIC_MIN: usize = 4;

/// What a stamp holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    /// The payload format ([`VERSION`]).
    pub stamp: u32,
    pub peer: Peer,
    /// The stamped config layer: any `config.toml` keys.
    #[serde(default)]
    pub config: toml::Table,
}

/// How a stamped binary dials its hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    /// `ws://host:port` or `wss://…`.
    pub url: String,
    /// The controller's X25519 public key, hex.
    pub hub_key: String,
    /// This peer's id at the hub.
    pub id: String,
    /// The directory the agent works in, if not where it was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The `[sandbox]` preset `construct build --sandbox` laid in, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
}

impl Stamp {
    /// The payload and trailer to append to a binary.
    pub fn seal(&self) -> Result<Vec<u8>> {
        let payload = toml::to_string(self).context("encoding the stamp")?.into_bytes();
        if payload.len() > MAX_PAYLOAD {
            bail!("the stamp is {} bytes; the most a reader accepts is {MAX_PAYLOAD}", payload.len());
        }
        let len = u32::try_from(payload.len()).context("stamp length")?;
        let mut out = payload.clone();
        out.extend_from_slice(&Sha256::digest(&payload));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&MAGIC);
        Ok(out)
    }
}

/// Where a trailer says its payload is.
struct Located {
    start: u64,
    len: usize,
    sha: [u8; 32],
}

/// Read a trailer (the file's last bytes, up to [`TRAILER`]) against the
/// file's length: `None` when there is no stamp, an error when there is a
/// broken one.
fn locate(tail: &[u8], file_len: u64) -> Result<Option<Located>> {
    if !tail.ends_with(&MAGIC) {
        let cut = (CUT_MAGIC_MIN..MAGIC.len()).any(|k| MAGIC.get(..k).is_some_and(|m| tail.ends_with(m)));
        if cut {
            bail!("its stamp trailer is truncated (the file ends partway through the CNSTPEER marker)");
        }
        return Ok(None);
    }
    let (Some(sha), Some(len)) = (tail.get(..32), tail.get(32..36)) else {
        bail!("its stamp trailer is truncated ({file_len} bytes is shorter than a trailer)");
    };
    let len = u32::from_le_bytes(len.try_into().context("stamp length")?) as usize;
    let need = len as u64 + TRAILER as u64;
    if len == 0 || len > MAX_PAYLOAD || need > file_len {
        bail!("its stamp trailer is corrupt or truncated (payload length {len} does not fit a {file_len}-byte file)");
    }
    Ok(Some(Located { start: file_len - need, len, sha: sha.try_into().context("stamp checksum")? }))
}

/// Check a payload against its trailer's checksum, then parse it.
fn open(payload: &[u8], sha: &[u8; 32]) -> Result<Stamp> {
    if Sha256::digest(payload).as_slice() != sha {
        bail!("its stamp payload fails its SHA-256 checksum (the binary is corrupt; build it again)");
    }
    let text = std::str::from_utf8(payload).context("its stamp payload is not UTF-8")?;
    let stamp: Stamp = toml::from_str(text).context("its stamp payload does not parse")?;
    if stamp.stamp != VERSION {
        bail!("its stamp is format {}; this build reads format {VERSION}", stamp.stamp);
    }
    Ok(stamp)
}

/// The binary without its stamp, and the stamp, from a whole file in memory.
/// `construct build` strips a stamp before laying a new one on.
pub fn split(bytes: &[u8]) -> Result<(&[u8], Option<Stamp>)> {
    let tail = bytes.get(bytes.len().saturating_sub(TRAILER)..).unwrap_or(bytes);
    let Some(at) = locate(tail, bytes.len() as u64)? else {
        return Ok((bytes, None));
    };
    let start = usize::try_from(at.start).context("stamp offset")?;
    let payload = bytes.get(start..start + at.len).context("stamp payload out of range")?;
    let stamp = open(payload, &at.sha)?;
    Ok((bytes.get(..start).context("stamp offset out of range")?, Some(stamp)))
}

/// The stamp of the binary at `path`, reading only its last bytes.
pub fn read(path: &Path) -> Result<Option<Stamp>> {
    let fail = || format!("{} is a stamped construct whose stamp cannot be trusted", path.display());
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let file_len = f.metadata()?.len();
    let n = file_len.min(TRAILER as u64);
    let mut tail = vec![0; n as usize];
    f.seek(SeekFrom::Start(file_len - n))?;
    f.read_exact(&mut tail)?;
    let Some(at) = locate(&tail, file_len).with_context(fail)? else {
        return Ok(None);
    };
    let mut payload = vec![0; at.len];
    f.seek(SeekFrom::Start(at.start))?;
    f.read_exact(&mut payload)?;
    open(&payload, &at.sha).with_context(fail).map(Some)
}

/// This process's own binary: `current_exe()`, or `argv[0]` where that
/// fails (no `/proc`).
fn own_binary() -> Option<PathBuf> {
    std::env::current_exe().ok().or_else(|| std::env::args_os().next().map(PathBuf::from))
}

/// The stamp of the running binary, read once. A binary that cannot be
/// found (it was replaced or deleted under a running process) is unstamped.
pub fn current() -> Result<Option<&'static Stamp>> {
    static CURRENT: OnceLock<Result<Option<Stamp>, String>> = OnceLock::new();
    let got = CURRENT.get_or_init(|| match own_binary() {
        Some(path) if path.is_file() => read(&path).map_err(|e| format!("{e:#}")),
        _ => Ok(None),
    });
    match got {
        Ok(s) => Ok(s.as_ref()),
        Err(e) => bail!("{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> Stamp {
        let config: toml::Table = toml::from_str("llm = \"hub\"\n[sandbox]\nmode = \"read-only\"\n").unwrap();
        Stamp {
            stamp: VERSION,
            peer: Peer {
                url: "ws://10.0.0.2:7300".into(),
                hub_key: "ab".repeat(32),
                id: "old-pc".into(),
                cwd: None,
                sandbox: Some("readonly-host".into()),
            },
            config,
        }
    }

    fn stamped() -> Vec<u8> {
        let mut bin = b"\x7fELF pretend binary".to_vec();
        bin.extend(stamp().seal().unwrap());
        bin
    }

    #[test]
    fn a_sealed_stamp_reads_back_and_strips_to_the_binary() {
        let bytes = stamped();
        let (bin, got) = split(&bytes).unwrap();
        assert_eq!(bin, b"\x7fELF pretend binary");
        assert_eq!(got, Some(stamp()));
    }

    #[test]
    fn no_trailer_is_unstamped() {
        let (bin, got) = split(b"plain binary").unwrap();
        assert_eq!(bin, b"plain binary");
        assert!(got.is_none());
        assert!(split(b"").unwrap().1.is_none());
    }

    #[test]
    fn a_flipped_payload_byte_fails_the_checksum() {
        let mut bytes = stamped();
        let at = bytes.windows(6).position(|w| w == b"old-pc").unwrap();
        bytes[at] = b'n';
        let err = format!("{:#}", split(&bytes).unwrap_err());
        assert!(err.contains("SHA-256"), "{err}");
    }

    #[test]
    fn a_length_past_the_file_is_corrupt() {
        let mut bytes = stamped();
        let n = bytes.len();
        bytes[n - 12..n - 8].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = format!("{:#}", split(&bytes).unwrap_err());
        assert!(err.contains("corrupt or truncated"), "{err}");
    }

    #[test]
    fn a_trailer_cut_short_is_truncated_not_unstamped() {
        let bytes = stamped();
        let err = format!("{:#}", split(&bytes[..bytes.len() - 3]).unwrap_err());
        assert!(err.contains("truncated"), "{err}");
    }

    #[test]
    fn a_newer_format_fails_closed() {
        let mut s = stamp();
        s.stamp = VERSION + 1;
        let mut bytes = b"bin".to_vec();
        bytes.extend(s.seal().unwrap());
        assert!(format!("{:#}", split(&bytes).unwrap_err()).contains("format 2"));
    }

    #[test]
    fn the_file_reader_matches_the_memory_one() {
        let dir = std::env::temp_dir().join(format!("stamp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bin");
        std::fs::write(&path, stamped()).unwrap();
        assert_eq!(read(&path).unwrap(), Some(stamp()));
        std::fs::write(&path, b"plain").unwrap();
        assert_eq!(read(&path).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
