//! Static X25519 keypairs, public keys, and Noise handshake hashes (#368).
//!
//! Kept separate from the Noise IK handshake state machine (`noise.rs`) so
//! identity/SAS helpers and key zeroization stay focused and easy to audit.

use crate::crypto::noise_builder;
use crate::error::WireError;
use std::fmt;

/// Length of an X25519 static key (public or private) in bytes.
pub const KEY_LEN: usize = 32;

/// Length of the Noise SHA-256 handshake hash (`h`) in bytes.
pub const HASH_LEN: usize = 32;

/// A 32-byte X25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn try_from_slice(slice: &[u8]) -> Result<Self, WireError> {
        let arr: [u8; KEY_LEN] = slice
            .try_into()
            .map_err(|_| WireError::Crypto(format!("expected {KEY_LEN}-byte public key, got {}", slice.len())))?;
        Ok(Self(arr))
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Lowercase hex encoding (64 ASCII chars).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Parse a 64-char hex X25519 public key.
    pub fn from_hex(s: &str) -> Result<Self, WireError> {
        let bytes = hex_decode_32(s)?;
        Ok(Self(bytes))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A random single-use secret of `bytes` bytes from the OS CSPRNG (ring), as
/// lowercase hex: a join token (#361).
pub fn random_token(bytes: usize) -> Result<String, WireError> {
    use ring::rand::SecureRandom as _;
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new().fill(&mut buf).map_err(|_| WireError::Crypto("OS random source failed".into()))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// An X25519 static keypair (32-byte private + 32-byte public).
///
/// The private key is redacted from `Debug` and zeroed on `Drop` so accidental
/// logging or freed heap memory does not leak static key material.
#[derive(Clone)]
pub struct Keypair {
    private: [u8; KEY_LEN],
    public: PublicKey,
}

impl Keypair {
    /// Generate a fresh X25519 static keypair using ring's OS CSPRNG.
    pub fn generate() -> Result<Self, WireError> {
        let raw =
            noise_builder()?.generate_keypair().map_err(|e| WireError::Crypto(format!("generate keypair: {e}")))?;
        let private: [u8; KEY_LEN] =
            raw.private.try_into().map_err(|_| WireError::Crypto("unexpected private key length".into()))?;
        let public = PublicKey::try_from_slice(&raw.public)?;
        Ok(Self { private, public })
    }

    /// Construct from persisted raw bytes (e.g. `$CONSTRUCT_HOME/hub.key`).
    #[must_use]
    pub const fn from_bytes(private: [u8; KEY_LEN], public: PublicKey) -> Self {
        Self { private, public }
    }

    /// Rebuild a keypair from its private half alone, deriving the public key
    /// (X25519 base-point multiplication with clamping), so a key file only
    /// has to hold the 32 private bytes.
    #[must_use]
    pub fn from_private(private: [u8; KEY_LEN]) -> Self {
        let public = curve25519_dalek::montgomery::MontgomeryPoint::mul_base_clamped(private).to_bytes();
        Self { private, public: PublicKey(public) }
    }

    #[must_use]
    pub const fn public(&self) -> PublicKey {
        self.public
    }

    #[must_use]
    pub const fn private_bytes(&self) -> &[u8; KEY_LEN] {
        &self.private
    }
}

impl Drop for Keypair {
    fn drop(&mut self) {
        // Volatile write so the compiler cannot elide clearing key material.
        for byte in &mut self.private {
            // SAFETY: `byte` is a valid, aligned mutable reference to a `u8`
            // owned by `self`.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl fmt::Debug for Keypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keypair").field("public", &self.public).field("private", &"[redacted]").finish()
    }
}

/// The 32-byte Noise handshake hash (`h`) binding both static keys, both
/// ephemeral keys, and the prologue. Upper layers (#361) derive SAS codes
/// directly from this value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandshakeHash([u8; HASH_LEN]);

impl HandshakeHash {
    pub fn try_from_slice(slice: &[u8]) -> Result<Self, WireError> {
        let arr: [u8; HASH_LEN] = slice
            .try_into()
            .map_err(|_| WireError::Crypto(format!("expected {HASH_LEN}-byte handshake hash, got {}", slice.len())))?;
        Ok(Self(arr))
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HASH_LEN] {
        &self.0
    }

    /// Short human-readable SAS code (`XXX-XXX`, 30 bits from an unambiguous
    /// Crockford-like alphabet) derived from the handshake hash (#361).
    #[must_use]
    pub fn sas_code(&self) -> String {
        const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
        let bits = u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]]);
        let mut chars = [b'-'; 7];
        for (i, slot) in chars.iter_mut().take(3).enumerate() {
            let idx = ((bits >> (27 - i * 5)) & 0x1f) as usize;
            *slot = ALPHABET[idx];
        }
        for (i, slot) in chars.iter_mut().skip(4).enumerate() {
            let idx = ((bits >> (12 - i * 5)) & 0x1f) as usize;
            *slot = ALPHABET[idx];
        }
        String::from_utf8(chars.to_vec()).unwrap_or_default()
    }
}

impl fmt::Debug for HandshakeHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HandshakeHash({})", hex_encode(&self.0))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode_32(s: &str) -> Result<[u8; KEY_LEN], WireError> {
    let raw = s.as_bytes();
    if raw.len() != KEY_LEN * 2 {
        return Err(WireError::Protocol(format!("expected {} hex chars, got {}", KEY_LEN * 2, raw.len())));
    }
    let mut out = [0u8; KEY_LEN];
    for i in 0..KEY_LEN {
        let hi = hex_nibble(raw[i * 2])?;
        let lo = hex_nibble(raw[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, WireError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(WireError::Protocol(format!("invalid hex character {:?}", b as char))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_hex_round_trip_and_redacted_debug() {
        let kp = Keypair::generate().unwrap();
        let hex = kp.public().to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(PublicKey::from_hex(&hex).unwrap(), kp.public());
        let dbg = format!("{kp:?}");
        assert!(dbg.contains("[redacted]"), "{dbg}");
    }

    #[test]
    fn from_private_derives_the_generated_public_key() {
        let kp = Keypair::generate().unwrap();
        let again = Keypair::from_private(*kp.private_bytes());
        assert_eq!(again.public(), kp.public());
    }

    #[test]
    fn random_tokens_are_hex_and_differ() {
        let (a, b) = (random_token(16).unwrap(), random_token(16).unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn sas_code_is_deterministic_and_formatted() {
        let h = HandshakeHash([0xAB; HASH_LEN]);
        let code = h.sas_code();
        assert_eq!(code.len(), 7);
        assert_eq!(code.as_bytes()[3], b'-');
        assert_eq!(code, h.sas_code());
    }
}
