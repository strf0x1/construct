//! `construct build` (#363): turn a construct binary into a drop-and-run peer
//! for one hub by appending a stamp (`construct_core::config::stamp`). The
//! source is a local binary (`--from`, or this one for the host's own
//! target) or a published release for another target, fetched once into
//! `$CONSTRUCT_HOME/cache/release/<version>/` and checked against its
//! published SHA-256 before it is used, every time it is used.
//!
//! The design choice: the stamp carries policy and where to dial, never a
//! secret. The controller key is public, the join token is handed over
//! separately (`CONSTRUCT_JOIN=…`), and model calls default to the hub
//! (`llm = "hub"`, ADR 0072), so a copied binary opens nothing by itself.

use crate::registry::check_id;
use anyhow::{Context, Result, bail};
use construct_core::config::stamp::{self, Peer, Stamp};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where releases are downloaded from (`CONSTRUCT_RELEASE_URL` overrides).
pub const RELEASE_URL: &str = "https://github.com/strf0x1/construct/releases/download";
/// The release profiles published as raw, stampable binaries.
pub const PROFILES: &[&str] = &["bare"];

/// A `[sandbox]` preset `--sandbox` bakes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Read and write the working directory (the default policy).
    Workspace,
    /// Read the host's configuration, logs and process tables; every write
    /// outside construct's temp dir asks first. For a diagnostic peer.
    ReadonlyHost,
    /// No filesystem boundary.
    Off,
}

/// What `readonly-host` adds to the readable paths; a path that does not
/// exist on the target is dropped when the policy is built.
const HOST_READ: &[&str] = &["/etc", "/var/log", "/var/lib", "/proc", "/sys", "/run", "/usr", "/opt", "/srv", "/boot"];

impl std::str::FromStr for Preset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "workspace" => Ok(Self::Workspace),
            "readonly-host" => Ok(Self::ReadonlyHost),
            "off" => Ok(Self::Off),
            _ => Err(format!("unknown sandbox preset {s:?} (workspace, readonly-host, off)")),
        }
    }
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::ReadonlyHost => "readonly-host",
            Self::Off => "off",
        }
    }

    /// The `[sandbox]` table this preset lays in.
    fn table(self) -> toml::Table {
        let mut t = toml::Table::new();
        let mode = match self {
            Self::Workspace => "workspace",
            Self::ReadonlyHost => "read-only",
            Self::Off => "off",
        };
        t.insert("mode".into(), mode.into());
        if self == Self::ReadonlyHost {
            let read = HOST_READ.iter().map(|p| toml::Value::from(*p)).collect();
            t.insert("allow_read".into(), toml::Value::Array(read));
        }
        t
    }
}

/// Everything a stamp is made from.
pub struct Spec {
    pub url: String,
    pub hub_key: String,
    pub id: String,
    pub cwd: Option<String>,
    pub sandbox: Option<Preset>,
    /// Extra `config.toml` keys to stamp (`--settings`).
    pub settings: toml::Table,
}

impl Spec {
    /// The stamp: the settings, `llm = "hub"` unless they say otherwise, and
    /// the sandbox preset over them.
    pub fn stamp(self) -> Result<Stamp> {
        if !(self.url.starts_with("ws://") || self.url.starts_with("wss://")) {
            bail!("--url must be ws://host:port or wss://…, not {}", self.url);
        }
        check_id(&self.id).map_err(anyhow::Error::msg).context("--name")?;
        if self.hub_key.len() != 64 || !self.hub_key.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("the hub key must be 64 hex characters (`construct hub key` prints it)");
        }
        let mut config = self.settings;
        config.entry("llm").or_insert_with(|| "hub".into());
        if let Some(p) = self.sandbox {
            let sandbox = config.entry("sandbox").or_insert_with(|| toml::Table::new().into());
            let Some(table) = sandbox.as_table_mut() else { bail!("--settings has a `sandbox` that is not a table") };
            table.extend(p.table());
        }
        let peer = Peer {
            url: self.url,
            hub_key: self.hub_key.to_ascii_lowercase(),
            id: self.id,
            cwd: self.cwd,
            sandbox: self.sandbox.map(|p| p.name().to_string()),
        };
        Ok(Stamp { stamp: stamp::VERSION, peer, config })
    }
}

/// Write `source` with `stamp` appended to `out` (mode `0755`), replacing a
/// stamp the source already carries. A source whose stamp is broken is
/// refused rather than stamped twice.
pub fn write(source: &[u8], stamp: &Stamp, out: &Path) -> Result<()> {
    let (binary, _) = stamp::split(source).context("the source binary")?;
    let mut bytes = binary.to_vec();
    bytes.extend(stamp.seal()?);
    let tmp = out.with_extension("stamping");
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, out).with_context(|| format!("moving the stamped binary to {}", out.display()))?;
    // Read it back the way the peer will, so a bad stamp fails here, not there.
    match stamp::read(out)? {
        Some(s) if &s == stamp => Ok(()),
        _ => bail!("{} does not read back as the stamp just written", out.display()),
    }
}

/// The published file name of a release binary.
pub fn release_name(target: &str, profile: &str) -> String {
    format!("construct-{target}-{profile}")
}

/// A release binary for `target`, from the cache or downloaded into it,
/// checked against its published SHA-256 either way.
pub async fn release(home: &Path, version: &str, target: &str, profile: &str) -> Result<PathBuf> {
    if !PROFILES.contains(&profile) {
        bail!("no `{profile}` release profile; published profiles: {}", PROFILES.join(", "));
    }
    let name = release_name(target, profile);
    let dir = home.join("cache").join("release").join(version);
    let (bin, sum) = (dir.join(&name), dir.join(format!("{name}.sha256")));
    if let (Ok(bytes), Ok(line)) = (std::fs::read(&bin), std::fs::read_to_string(&sum)) {
        verify(&bytes, &line, &name).with_context(|| format!("the cached {}", bin.display()))?;
        return Ok(bin);
    }
    let base = std::env::var("CONSTRUCT_RELEASE_URL").ok().filter(|s| !s.is_empty());
    let base = base.as_deref().unwrap_or(RELEASE_URL).trim_end_matches('/');
    let url = format!("{base}/v{version}/{name}");
    let http = construct_core::http::build(construct_core::http::engine())?;
    let line = fetch(&http, &format!("{url}.sha256")).await?;
    let line = String::from_utf8(line).context("the published checksum is not text")?;
    let bytes = fetch(&http, &url).await?;
    verify(&bytes, &line, &name).with_context(|| format!("downloaded {url}"))?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&bin, &bytes)?;
    std::fs::write(&sum, &line)?;
    Ok(bin)
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = http.get(url).send().await.with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("fetching {url}: {} (is there a release for this version and target?)", resp.status());
    }
    Ok(resp.bytes().await.with_context(|| format!("reading {url}"))?.to_vec())
}

/// Check `bytes` against a `shasum -a 256` line (`<hex>  <name>`).
fn verify(bytes: &[u8], line: &str, name: &str) -> Result<()> {
    let mut words = line.split_whitespace();
    let want = words.next().unwrap_or_default().to_ascii_lowercase();
    if let Some(file) = words.next()
        && file.trim_start_matches('*') != name
    {
        bail!("the checksum file names {file}, not {name}");
    }
    let got: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if want != got {
        bail!("SHA-256 mismatch: published {want}, got {got}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            url: "ws://10.0.0.2:7300".into(),
            hub_key: "AB".repeat(32),
            id: "old-pc".into(),
            cwd: None,
            sandbox: None,
            settings: toml::Table::new(),
        }
    }

    #[test]
    fn a_stamp_defaults_to_the_hub_for_models() {
        let s = spec().stamp().unwrap();
        assert_eq!(s.config["llm"].as_str(), Some("hub"));
        assert_eq!(s.peer.hub_key, "ab".repeat(32));
        let mut own = spec();
        own.settings = toml::from_str("llm = \"local\"\n").unwrap();
        assert_eq!(own.stamp().unwrap().config["llm"].as_str(), Some("local"));
    }

    #[test]
    fn readonly_host_reads_the_host_and_writes_nothing() {
        let mut s = spec();
        s.sandbox = Some(Preset::ReadonlyHost);
        s.settings = toml::from_str("[sandbox]\nmemory_max = \"2G\"\nmode = \"workspace\"\n").unwrap();
        let st = s.stamp().unwrap();
        let sb = st.config["sandbox"].as_table().unwrap();
        assert_eq!(sb["mode"].as_str(), Some("read-only"));
        assert_eq!(sb["memory_max"].as_str(), Some("2G"));
        assert!(sb["allow_read"].as_array().unwrap().iter().any(|v| v.as_str() == Some("/var/log")));
        assert_eq!(st.peer.sandbox.as_deref(), Some("readonly-host"));
    }

    #[test]
    fn a_bad_url_id_or_key_is_refused() {
        let mut s = spec();
        s.url = "http://x".into();
        assert!(s.stamp().is_err());
        let mut s = spec();
        s.id = "bad id".into();
        assert!(s.stamp().is_err());
        let mut s = spec();
        s.hub_key = "zz".into();
        assert!(s.stamp().is_err());
    }

    #[test]
    fn restamping_replaces_the_old_stamp() {
        let dir = std::env::temp_dir().join(format!("hub-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("construct-old-pc");
        let first = spec().stamp().unwrap();
        write(b"binary", &first, &out).unwrap();
        let mut again = spec();
        again.id = "new-pc".into();
        let second = again.stamp().unwrap();
        write(&std::fs::read(&out).unwrap(), &second, &out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let (bin, got) = stamp::split(&bytes).unwrap();
        assert_eq!(bin, b"binary");
        assert_eq!(got.unwrap().peer.id, "new-pc");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_checksum_line_must_match_name_and_bytes() {
        let sum: String = Sha256::digest(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        verify(b"abc", &format!("{sum}  construct-x-bare\n"), "construct-x-bare").unwrap();
        verify(b"abc", &sum, "construct-x-bare").unwrap();
        assert!(verify(b"abd", &sum, "construct-x-bare").is_err());
        assert!(verify(b"abc", &format!("{sum}  other"), "construct-x-bare").is_err());
    }
}
