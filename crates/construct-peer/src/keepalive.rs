//! Application-level keep-alive and jittered reconnect backoff (#368).
//!
//! Keep-alive runs inside the encrypted Noise `control` channel (`Ping`/`Pong`)
//! rather than relying only on WebSocket-layer pings, because middleboxes and
//! tunnels can answer or silently drop transport pings while the peer process
//! is stalled. Backoff uses capped exponential delay with deterministic or
//! clock-seeded jitter so a thundering herd of peers never reconnects in
//! lockstep after a hub restart.

use crate::envelope::{Control, Envelope};
use crate::error::WireError;
use crate::frame::FrameStream;
use crate::noise::PeerLink;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default interval between encrypted `Control::Ping` probes (~25 s per #368).
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);

/// Default deadline for receiving a matching `Control::Pong` before declaring
/// the link dead.
pub const DEFAULT_PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// Timing parameters for [`keepalive_recv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepAlive {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self { interval: DEFAULT_PING_INTERVAL, timeout: DEFAULT_PONG_TIMEOUT }
    }
}

/// State of an in-flight keep-alive probe on one `PeerLink`.
#[derive(Debug, Default)]
pub struct KeepAliveState {
    next_nonce: u64,
    pending_nonce: Option<u64>,
}

impl KeepAliveState {
    #[must_use]
    pub const fn new() -> Self {
        Self { next_nonce: 1, pending_nonce: None }
    }
}

/// Receive the next non-keepalive envelope while transparently answering
/// incoming `Ping`s and issuing periodic `Ping`s on idle links.
///
/// Returns `Err(WireError::Timeout("keep-alive pong"))` when the remote fails
/// to answer a `Ping` within `cfg.timeout`.
pub async fn recv_with_keepalive<S: FrameStream>(
    link: &mut PeerLink<S>,
    cfg: KeepAlive,
    state: &mut KeepAliveState,
) -> Result<Envelope, WireError> {
    loop {
        let wait = if state.pending_nonce.is_some() { cfg.timeout } else { cfg.interval };
        match tokio::time::timeout(wait, link.recv()).await {
            Ok(Ok(env)) => {
                if env.channel == crate::envelope::Channel::Control
                    && let Ok(ctrl) = env.decode_control()
                {
                    match ctrl {
                        Control::Ping { nonce } => {
                            let pong = Envelope::control(&Control::Pong { nonce })?;
                            link.send(&pong).await?;
                            continue;
                        }
                        Control::Pong { nonce } => {
                            if state.pending_nonce == Some(nonce) {
                                state.pending_nonce = None;
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
                // Any valid authenticated envelope proves liveness.
                state.pending_nonce = None;
                return Ok(env);
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                if state.pending_nonce.is_some() {
                    return Err(WireError::Timeout("keep-alive pong"));
                }
                let nonce = state.next_nonce;
                state.next_nonce = state.next_nonce.wrapping_add(1).max(1);
                state.pending_nonce = Some(nonce);
                let ping = Envelope::control(&Control::Ping { nonce })?;
                link.send(&ping).await?;
            }
        }
    }
}

/// Capped exponential backoff with ±25% jitter for peer reconnection.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
    rng_state: u64,
}

impl Backoff {
    /// Create a backoff starting at `base`, doubling up to `max`, seeded from
    /// the system clock.
    #[must_use]
    pub fn new(base: Duration, max: Duration) -> Self {
        let seed =
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9e37_79b9_7f4a_7c15);
        Self::with_seed(base, max, seed)
    }

    /// Deterministic constructor for reproducible tests.
    #[must_use]
    pub fn with_seed(base: Duration, max: Duration, seed: u64) -> Self {
        let max = max.max(base);
        Self { base, max, attempt: 0, rng_state: seed | 1 }
    }

    /// Reset backoff after a session successfully establishes and runs.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Current failure attempt count since last [`reset`](Self::reset).
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Compute the next sleep duration and advance the attempt counter.
    ///
    /// Jitter scales the exponential delay within `[0.75 * d, 1.25 * d]`,
    /// clamped to `max`.
    pub fn next_delay(&mut self) -> Duration {
        let shift = self.attempt.min(20);
        self.attempt = self.attempt.saturating_add(1);
        let exp = self.base.checked_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX)).unwrap_or(self.max).min(self.max);
        let r = xorshift64(&mut self.rng_state);
        // Factor in [750, 1250] permille (±25% jitter).
        let permille = 750 + (r % 501);
        let nanos = (exp.as_nanos().saturating_mul(u128::from(permille))) / 1000;
        let clamped = nanos.min(self.max.as_nanos());
        Duration::from_nanos(u64::try_from(clamped).unwrap_or(u64::MAX))
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(30))
    }
}

/// Dial repeatedly with jittered backoff until `dial` returns `Ok` or
/// `max_attempts` (when `Some`) is exhausted.
///
/// Non-retryable authentication errors (`WireError::Rejected`) abort
/// immediately instead of hammering the hub.
pub async fn connect_with_backoff<T, F, Fut>(
    backoff: &mut Backoff,
    max_attempts: Option<u32>,
    mut dial: F,
) -> Result<T, WireError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, WireError>>,
{
    let mut attempt = 0u32;
    loop {
        attempt = attempt.saturating_add(1);
        match dial(attempt).await {
            Ok(v) => {
                backoff.reset();
                return Ok(v);
            }
            Err(e @ WireError::Rejected(_)) => return Err(e),
            Err(e) => {
                if max_attempts.is_some_and(|m| attempt >= m) {
                    return Err(e);
                }
                let delay = backoff.next_delay();
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_within_jitter_and_caps() {
        let base = Duration::from_millis(100);
        let max = Duration::from_secs(1);
        let mut b = Backoff::with_seed(base, max, 42);
        let d0 = b.next_delay();
        let d1 = b.next_delay();
        let d2 = b.next_delay();
        assert!(d0 >= Duration::from_millis(75) && d0 <= Duration::from_millis(125), "{d0:?}");
        assert!(d1 >= Duration::from_millis(150) && d1 <= Duration::from_millis(250), "{d1:?}");
        assert!(d2 >= Duration::from_millis(300) && d2 <= Duration::from_millis(500), "{d2:?}");
        for _ in 0..10 {
            let d = b.next_delay();
            assert!(d <= max, "{d:?} > {max:?}");
        }
        b.reset();
        assert_eq!(b.attempts(), 0);
    }
}
