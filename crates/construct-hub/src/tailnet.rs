//! This machine's Tailscale address (#364), for `construct hub --listen
//! tailscale` and `construct build --tailscale`: ask `tailscale status
//! --json` and take the node's own tailnet IPv4 and MagicDNS name.
//!
//! The design choice: the URL a peer dials uses the tailnet IPv4, not the
//! MagicDNS name, because the IP works on every tailnet node whether or not
//! its resolver goes through Tailscale; the name is shown alongside for
//! people. No TLS: the link is Noise IK end to end (ADR 0068), so
//! `ws://<tailnet ip>:<port>` needs no certificate, domain or extra daemon.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::process::Command;

/// Where the CLI lives when it is not on `PATH` (the macOS app bundles it).
const CANDIDATES: &[&str] = &["tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"];

/// This node on its tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The node's tailnet IPv4 (`100.x.y.z`).
    pub ip: Ipv4Addr,
    /// Its MagicDNS name without the trailing dot, when MagicDNS is on.
    pub name: Option<String>,
}

impl Node {
    /// The URL a peer dials for a hub listening on `port` here.
    pub fn url(&self, port: u16) -> String {
        format!("ws://{}:{port}", self.ip)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(rename = "Self")]
    me: Option<Me>,
    current_tailnet: Option<Tailnet>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Me {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Tailnet {
    #[serde(rename = "MagicDNSEnabled", default)]
    magic_dns: bool,
}

/// Read `tailscale status --json`'s output.
pub fn parse(json: &str) -> Result<Node> {
    let s: Status = serde_json::from_str(json).context("`tailscale status --json` printed something unexpected")?;
    if s.backend_state != "Running" {
        bail!("Tailscale is {} on this machine; run `tailscale up` first", s.backend_state);
    }
    let me = s.me.context("`tailscale status --json` has no Self node")?;
    let ip = me
        .tailscale_ips
        .iter()
        .find_map(|a| a.parse::<Ipv4Addr>().ok())
        .context("this node has no tailnet IPv4 address")?;
    let magic = s.current_tailnet.is_some_and(|t| t.magic_dns);
    let name = Some(me.dns_name.trim_end_matches('.').to_string()).filter(|n| magic && !n.is_empty());
    Ok(Node { ip, name })
}

/// Ask the local Tailscale CLI for this node.
pub fn detect() -> Result<Node> {
    for cli in CANDIDATES {
        let Ok(out) = Command::new(cli).args(["status", "--json"]).output() else { continue };
        if !out.status.success() && out.stdout.is_empty() {
            bail!("`{cli} status --json` failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        return parse(&String::from_utf8_lossy(&out.stdout));
    }
    bail!("no `tailscale` CLI found; install Tailscale, or give the address yourself")
}

/// `tailscale` or `tailscale:<port>` as a listen address: the tailnet IPv4
/// with that port (else `default_port`). `None` for any other address.
pub fn listen_port(spec: &str, default_port: u16) -> Option<Result<u16>> {
    let rest = spec.strip_prefix("tailscale")?;
    Some(match rest.strip_prefix(':') {
        None if rest.is_empty() => Ok(default_port),
        None => return None,
        Some(p) => p.parse().with_context(|| format!("{spec}: {p:?} is not a port")),
    })
}

/// A listen address as given: `tailscale[:<port>]` becomes this node's
/// tailnet IPv4 and that port (else `default_port`); anything else is kept.
pub fn resolve_listen(spec: &str, default_port: u16) -> Result<String> {
    match listen_port(spec, default_port) {
        None => Ok(spec.to_string()),
        Some(port) => Ok(format!("{}:{}", detect()?.ip, port?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = r#"{"BackendState":"Running","Self":{"DNSName":"hub-box.tail1234.ts.net.",
        "TailscaleIPs":["fd7a:115c:a1e0::1","100.84.1.2"]},"CurrentTailnet":{"MagicDNSEnabled":true},"Peer":{}}"#;

    #[test]
    fn a_running_node_gives_its_ipv4_and_magicdns_name() {
        let n = parse(RUNNING).unwrap();
        assert_eq!(n.ip, Ipv4Addr::new(100, 84, 1, 2));
        assert_eq!(n.name.as_deref(), Some("hub-box.tail1234.ts.net"));
        assert_eq!(n.url(9443), "ws://100.84.1.2:9443");
    }

    #[test]
    fn without_magicdns_there_is_no_name() {
        let n = parse(&RUNNING.replace("\"MagicDNSEnabled\":true", "\"MagicDNSEnabled\":false")).unwrap();
        assert_eq!(n.name, None);
    }

    #[test]
    fn a_stopped_or_logged_out_node_says_so() {
        let e = parse(r#"{"BackendState":"NeedsLogin","Self":null}"#).unwrap_err().to_string();
        assert!(e.contains("NeedsLogin") && e.contains("tailscale up"), "{e}");
        let e = parse(&RUNNING.replace(",\"100.84.1.2\"", "")).unwrap_err().to_string();
        assert!(e.contains("IPv4"), "{e}");
    }

    #[test]
    fn listen_specs() {
        assert_eq!(listen_port("tailscale", 9443).unwrap().unwrap(), 9443);
        assert_eq!(listen_port("tailscale:7300", 9443).unwrap().unwrap(), 7300);
        assert!(listen_port("tailscale:x", 9443).unwrap().is_err());
        assert!(listen_port("tailscaled:1", 9443).is_none());
        assert!(listen_port("100.84.1.2:9443", 9443).is_none());
    }
}
