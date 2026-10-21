//! Snapshot in, atomic write out. The edit pipeline works on lines; this
//! module keeps the round trip faithful: EOL flavour, BOM, trailing newline
//! and file mode are captured on read and restored on write. Writes go
//! through a temp file in the same directory and a rename, so a crash never
//! leaves half a file.

use std::path::{Path, PathBuf};

const BOM: char = '\u{feff}';

pub struct Snapshot {
    /// The real path (symlinks resolved), so the rename replaces the target.
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub crlf: bool,
    pub bom: bool,
    pub trailing_newline: bool,
    pub permissions: std::fs::Permissions,
}

pub enum ReadError {
    Io(std::io::Error),
    Binary,
    Utf16,
    /// The bytes, handed back so a read can still show them.
    NotUtf8(Vec<u8>),
}

/// Split text into lines the way the anchor map counts them: CRLF and LF
/// both end a line, the final terminator isn't a line, and "" has no lines.
pub fn split(text: &str) -> (Vec<String>, bool) {
    let trailing = text.ends_with('\n');
    let body = text.replace("\r\n", "\n");
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    if trailing || body.is_empty() {
        lines.pop();
    }
    (lines, trailing)
}

/// Decode file bytes into lines, refusing what a line edit can't round-trip.
pub fn decode(bytes: Vec<u8>) -> Result<(String, bool), ReadError> {
    if super::super::super::fs::is_utf16(&bytes) {
        return Err(ReadError::Utf16);
    }
    if bytes.contains(&0) {
        return Err(ReadError::Binary);
    }
    let text = String::from_utf8(bytes).map_err(|e| ReadError::NotUtf8(e.into_bytes()))?;
    match text.strip_prefix(BOM) {
        Some(rest) => Ok((rest.to_string(), true)),
        None => Ok((text, false)),
    }
}

pub fn read(path: &Path) -> Result<Snapshot, ReadError> {
    let path = std::fs::canonicalize(path).map_err(ReadError::Io)?;
    let permissions = std::fs::metadata(&path).map_err(ReadError::Io)?.permissions();
    let (text, bom) = decode(std::fs::read(&path).map_err(ReadError::Io)?)?;
    // EOL flavour from the first line break; a mixed file is normalized to it.
    let crlf = text.find('\n').is_some_and(|i| i > 0 && text.as_bytes()[i - 1] == b'\r');
    let (lines, trailing_newline) = split(&text);
    Ok(Snapshot { path, lines, crlf, bom, trailing_newline, permissions })
}

pub fn write(s: &Snapshot, lines: &[String]) -> std::io::Result<()> {
    let eol = if s.crlf { "\r\n" } else { "\n" };
    let mut text = String::new();
    if s.bom {
        text.push(BOM);
    }
    text.push_str(&lines.join(eol));
    if s.trailing_newline && !lines.is_empty() {
        text.push_str(eol);
    }
    let name = s.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let tmp = s.path.with_file_name(format!(".{name}.hashline-{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    let done = std::fs::set_permissions(&tmp, s.permissions.clone()).and_then(|()| std::fs::rename(&tmp, &s.path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("construct-hashline-fsio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn file(d: &Dir, content: &[u8]) -> PathBuf {
        let p = d.0.join("f.txt");
        std::fs::write(&p, content).unwrap();
        p
    }

    fn snap(p: &Path) -> Snapshot {
        read(p).ok().expect("readable")
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lf_round_trip_keeps_the_trailing_newline() {
        let d = dir("lf");
        let p = file(&d, b"a\nb\nc\n");
        let s = snap(&p);
        assert_eq!((s.lines.clone(), s.crlf, s.trailing_newline), (v(&["a", "b", "c"]), false, true));
        write(&s, &v(&["a", "B", "c"])).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\nB\nc\n");
    }

    #[test]
    fn crlf_round_trip() {
        let d = dir("crlf");
        let p = file(&d, b"a\r\nb\r\n");
        let s = snap(&p);
        assert_eq!((s.lines.clone(), s.crlf), (v(&["a", "b"]), true));
        write(&s, &v(&["a", "b", "c"])).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn no_trailing_newline_round_trip() {
        let d = dir("nonl");
        let p = file(&d, b"a\nb");
        let s = snap(&p);
        assert_eq!((s.lines.clone(), s.trailing_newline), (v(&["a", "b"]), false));
        write(&s, &v(&["a", "b", "c"])).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\nb\nc");
    }

    #[test]
    fn a_bom_is_kept_and_not_part_of_line_one() {
        let d = dir("bom");
        let p = file(&d, "\u{feff}head\nbody\n".as_bytes());
        let s = snap(&p);
        assert!(s.bom);
        assert_eq!(s.lines, v(&["head", "body"]));
        write(&s, &v(&["HEAD", "body"])).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "\u{feff}HEAD\nbody\n");
    }

    #[test]
    fn zero_bytes_is_no_lines_and_a_lone_newline_is_one_blank_line() {
        assert!(split("").0.is_empty());
        assert_eq!(split("\n").0, [""]);
    }

    #[test]
    fn nul_bytes_and_invalid_utf8_are_refused() {
        assert!(matches!(decode(vec![b'a', 0, b'b']), Err(ReadError::Binary)));
        assert!(matches!(decode(vec![b'a', 0xff, b'b']), Err(ReadError::NotUtf8(b)) if b == [b'a', 0xff, b'b']));
        assert!(matches!(decode(vec![0xff, 0xfe, b'a', 0]), Err(ReadError::Utf16)));
    }

    #[test]
    fn mode_survives_the_atomic_write() {
        let d = dir("mode");
        let p = file(&d, b"#!/bin/sh\necho hi\n");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&snap(&p), &v(&["#!/bin/sh", "echo bye"])).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "#!/bin/sh\necho bye\n");
        assert_eq!(std::fs::read_dir(&d.0).unwrap().count(), 1, "no temp file left behind");
    }

    #[test]
    fn writing_through_a_symlink_replaces_the_target() {
        let d = dir("link");
        let p = file(&d, b"x\n");
        let link = d.0.join("link.txt");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        write(&snap(&link), &v(&["y"])).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "y\n");
    }

    #[test]
    fn writing_no_lines_leaves_an_empty_file() {
        let d = dir("empty");
        let p = file(&d, b"gone\n");
        write(&snap(&p), &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
    }
}
