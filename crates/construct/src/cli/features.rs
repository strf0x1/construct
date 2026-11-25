//! The Cargo features this binary was built with, as `construct manifest`
//! reports them: the one place a result or a size check can tell a build that
//! links the hub or the peer dialer from one that does not (ADR 0064, 0069).

/// This binary's Cargo features, for `construct manifest`.
pub(super) const FEATURES: &[&str] = &[
    #[cfg(feature = "full")]
    "full",
    #[cfg(feature = "lean")]
    "lean",
    #[cfg(feature = "keyring")]
    "keyring",
    #[cfg(feature = "web")]
    "web",
    #[cfg(feature = "memory")]
    "memory",
    #[cfg(feature = "skills")]
    "skills",
    #[cfg(feature = "history")]
    "history",
    #[cfg(feature = "jobs")]
    "jobs",
    #[cfg(feature = "deck")]
    "deck",
    #[cfg(feature = "peer")]
    "peer",
    #[cfg(feature = "hub")]
    "hub",
];
