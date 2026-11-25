//! Where the hub keeps its files inside `$CONSTRUCT_HOME` (#360).
//!
//! Declared once here so the server, the CLI and the tests agree. The control
//! socket's path must fit `sockaddr_un` (104 bytes on macOS, 108 on Linux), so
//! a very deep home is refused with a clear error rather than a bind failure.

use anyhow::{Context, Result};
use construct_peer::{Keypair, keyfile};
use std::path::{Path, PathBuf};

/// The longest socket path every supported platform accepts (macOS's
/// `sun_path` is 104 bytes including the trailing NUL).
pub const MAX_SOCKET_PATH: usize = 103;

/// The hub's files under one home.
#[derive(Debug, Clone)]
pub struct Files {
    pub home: PathBuf,
}

impl Files {
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self { home: home.to_path_buf() }
    }

    /// The controller's static X25519 private key (32 raw bytes, `0600`).
    /// Back it up: losing it orphans every deployed peer.
    #[must_use]
    pub fn key(&self) -> PathBuf {
        self.home.join("hub.key")
    }

    /// The controller's keypair, created on first use.
    pub fn keypair(&self) -> Result<Keypair> {
        keyfile::load_or_create(&self.key()).context("hub key")
    }

    /// The controller's public key (hex), if a key exists yet.
    pub fn public_key(&self) -> Result<Option<String>> {
        Ok(keyfile::load(&self.key()).context("hub key")?.map(|k| k.public().to_hex()))
    }

    /// The `0600` unix socket TUIs and CLI commands attach to.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.home.join("hub.sock")
    }

    /// Held with an exclusive OS lock while a hub runs; holds its pid.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.home.join("hub.lock")
    }

    /// The peer registry.
    #[must_use]
    pub fn registry(&self) -> PathBuf {
        self.home.join("hub").join("peers.json")
    }

    /// The socket path, or why it cannot be bound.
    pub fn checked_socket(&self) -> anyhow::Result<PathBuf> {
        let path = self.socket();
        let len = path.as_os_str().len();
        if len > MAX_SOCKET_PATH {
            anyhow::bail!(
                "the hub socket path {} is {len} bytes, longer than a unix socket allows ({MAX_SOCKET_PATH}); \
                 use a shorter CONSTRUCT_HOME",
                path.display()
            );
        }
        Ok(path)
    }
}
