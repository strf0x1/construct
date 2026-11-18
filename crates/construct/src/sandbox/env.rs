//! The environment a sandboxed process starts with: this process's, minus
//! anything that grants authority. The design choice: removal is by name
//! pattern as well as by list, so a credential nobody declared (a CI token, a
//! deploy key) is still kept from tools.

/// Names that look like credentials. Tools never see these, whatever the mode.
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    const MARKS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION"];
    MARKS.iter().any(|m| n.contains(m))
        && !matches!(
            n.as_str(),
            "SSH_AUTH_SOCK" | "XDG_SESSION_TYPE" | "XDG_SESSION_ID" | "XDG_SESSION_CLASS" | "XDG_SESSION_DESKTOP"
        )
}

/// Remove from `cmd`'s environment everything a tool must not inherit.
pub fn scrub_env(cmd: &mut tokio::process::Command) {
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str()
            && (is_secret_name(k) || crate::secrets::KNOWN.iter().any(|(n, _)| *n == k))
        {
            cmd.env_remove(k);
        }
    }
    // An agent with the user's ssh-agent can push anywhere the user can; one
    // with Deck's control gateway could steer tasks outside the `deck` tool.
    cmd.env_remove("SSH_AUTH_SOCK");
    for k in crate::config::deck::CONTROL_ENV {
        cmd.env_remove(k);
    }
}
