//! Ring + X25519 crypto resolver for `snow` Noise IK (#368).
//!
//! `snow`'s `RingResolver` handles AEAD (ChaChaPoly), SHA-256, and OS CSPRNG
//! via the `ring` already linked for `rustls`, but leaves `resolve_dh`
//! unimplemented because `ring::agreement` is ephemeral-only. Static X25519
//! Diffie-Hellman is supplied here by `curve25519-dalek` (`MontgomeryPoint`),
//! keeping snow's default-resolver cipher/hash crates out of the binary.

use crate::error::WireError;
use curve25519_dalek::montgomery::MontgomeryPoint;
use snow::Builder;
use snow::params::{CipherChoice, DHChoice, HashChoice};
use snow::resolvers::{CryptoResolver, RingResolver};
use snow::types::{Cipher, Dh, Hash, Random};

/// Noise handshake parameters shared by every peer and hub.
pub const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// Prologue mixed into the Noise handshake hash before message 1 so frames
/// from unrelated protocols never collide.
pub const NOISE_PROLOGUE: &[u8] = b"construct-peer-v1";

const DH_KEY_LEN: usize = 32;

/// Construct a `snow::Builder` backed by `RingResolver` (AEAD, SHA-256, OS
/// CSPRNG) plus static X25519 DH via `curve25519-dalek`.
pub(crate) fn noise_builder() -> Result<Builder<'static>, WireError> {
    let params = NOISE_PATTERN.parse().map_err(|e| WireError::Crypto(format!("parse noise params: {e}")))?;
    Ok(Builder::with_resolver(params, Box::new(PeerCryptoResolver)))
}

struct PeerCryptoResolver;

impl CryptoResolver for PeerCryptoResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        RingResolver.resolve_rng()
    }

    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        match choice {
            DHChoice::Curve25519 => Some(Box::<X25519Dh>::default()),
            _ => None,
        }
    }

    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        RingResolver.resolve_hash(choice)
    }

    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        RingResolver.resolve_cipher(choice)
    }
}

#[derive(Default)]
struct X25519Dh {
    privkey: [u8; DH_KEY_LEN],
    pubkey: [u8; DH_KEY_LEN],
}

impl X25519Dh {
    fn derive_pubkey(&mut self) {
        self.pubkey = MontgomeryPoint::mul_base_clamped(self.privkey).to_bytes();
    }
}

impl Drop for X25519Dh {
    fn drop(&mut self) {
        for byte in &mut self.privkey {
            // SAFETY: `byte` is a valid, aligned mutable reference owned by `self`.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl Dh for X25519Dh {
    fn name(&self) -> &'static str {
        "25519"
    }

    fn pub_len(&self) -> usize {
        DH_KEY_LEN
    }

    fn priv_len(&self) -> usize {
        DH_KEY_LEN
    }

    fn set(&mut self, privkey: &[u8]) {
        if let Some(src) = privkey.get(..DH_KEY_LEN) {
            self.privkey.copy_from_slice(src);
            self.derive_pubkey();
        }
    }

    fn generate(&mut self, rng: &mut dyn Random) -> Result<(), snow::Error> {
        rng.try_fill_bytes(&mut self.privkey)?;
        self.derive_pubkey();
        Ok(())
    }

    fn pubkey(&self) -> &[u8] {
        &self.pubkey
    }

    fn privkey(&self) -> &[u8] {
        &self.privkey
    }

    fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
        let Some(peer_bytes) = pubkey.get(..DH_KEY_LEN) else {
            return Err(snow::Error::Input);
        };
        let Some(dst) = out.get_mut(..DH_KEY_LEN) else {
            return Err(snow::Error::Input);
        };
        let mut peer = [0u8; DH_KEY_LEN];
        peer.copy_from_slice(peer_bytes);
        let shared = MontgomeryPoint(peer).mul_clamped(self.privkey).to_bytes();
        dst.copy_from_slice(&shared);
        Ok(())
    }
}
