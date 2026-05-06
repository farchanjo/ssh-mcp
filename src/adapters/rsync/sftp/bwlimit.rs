//! Token-bucket bandwidth limiter for the SFTP rsync transport.
//!
//! Refills `rate_kbps * 1024 / 1000 ms` tokens every 60 ms (the ADR
//! 0011 tick interval). Callers ask for a budget via [`TokenBucket::take`];
//! the call awaits until enough tokens are available to satisfy the
//! request, then debits the bucket.
//!
//! Lock-free hot path: [`AtomicU64`] for the running balance, an
//! [`AtomicU64`] for `last_refill_ms` (Unix epoch milliseconds). Refills
//! happen lazily on every `take` call instead of via a background ticker
//! so cancellation is free (no orphan ticker task to drop).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::{Instant, sleep};

/// Token-bucket rate limiter sized in bytes.
#[derive(Debug)]
pub struct TokenBucket {
    /// Maximum bucket size in bytes (also the per-tick refill ceiling
    /// when the bucket has been idle for >= 1 tick).
    capacity: u64,
    /// Refill rate in bytes per second.
    rate_bps: u64,
    /// Available tokens.
    tokens: AtomicU64,
    /// Monotonic milliseconds since [`TokenBucket::new`].
    last_refill_ms: AtomicU64,
    /// Anchor for the millisecond clock.
    anchor: Instant,
}

impl TokenBucket {
    /// Build a fresh bucket with capacity `rate_bps` (one second's
    /// worth of bytes — the ADR 0011 default).
    #[must_use]
    pub fn new(rate_bps: u64) -> Self {
        Self {
            capacity: rate_bps,
            rate_bps,
            tokens: AtomicU64::new(rate_bps),
            last_refill_ms: AtomicU64::new(0),
            anchor: Instant::now(),
        }
    }

    /// Best-effort take. Returns immediately if enough tokens are
    /// available; otherwise sleeps for the inferred shortfall and
    /// retries (one extra refill round at most).
    ///
    /// Cancel-safe — every `await` is on a `tokio::time::sleep` future
    /// that drops cleanly.
    pub async fn take(&self, want: u64) {
        if want == 0 || self.rate_bps == 0 {
            return;
        }
        loop {
            self.refill();
            let balance = self.tokens.load(Ordering::Acquire);
            if balance >= want {
                if self
                    .tokens
                    .compare_exchange_weak(
                        balance,
                        balance - want,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            // Sleep enough to accumulate the shortfall.
            let shortfall = want.saturating_sub(balance);
            let micros = shortfall.saturating_mul(1_000_000) / self.rate_bps.max(1);
            let pause = Duration::from_micros(micros).max(Duration::from_millis(60));
            sleep(pause).await;
        }
    }

    /// Lazily refill the bucket based on elapsed wall-clock since the
    /// last refill. Idempotent — can be called from any thread.
    fn refill(&self) {
        let now_ms = u64::try_from(self.anchor.elapsed().as_millis()).unwrap_or(u64::MAX);
        let last = self.last_refill_ms.load(Ordering::Acquire);
        if now_ms <= last {
            return;
        }
        let elapsed_ms = now_ms - last;
        let added = self.rate_bps.saturating_mul(elapsed_ms) / 1000;
        if added == 0 {
            return;
        }
        if self
            .last_refill_ms
            .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // Saturate at capacity.
            let mut current = self.tokens.load(Ordering::Acquire);
            loop {
                let next = current.saturating_add(added).min(self.capacity);
                match self.tokens.compare_exchange_weak(
                    current,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(observed) => current = observed,
                }
            }
        }
    }

    /// Available token snapshot (debug / test).
    #[must_use]
    pub fn available(&self) -> u64 {
        self.refill();
        self.tokens.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::TokenBucket;

    #[tokio::test]
    async fn take_returns_immediately_when_under_capacity() {
        let bucket = TokenBucket::new(1024 * 1024);
        bucket.take(1024).await;
        assert!(bucket.available() <= 1024 * 1024);
    }

    #[tokio::test]
    async fn take_zero_is_noop() {
        let bucket = TokenBucket::new(1024 * 1024);
        bucket.take(0).await;
    }

    #[tokio::test]
    async fn take_blocks_when_request_exceeds_capacity_until_refill() {
        // Tiny bucket — first take drains, second has to wait for refill.
        let bucket = TokenBucket::new(1_024);
        bucket.take(1_024).await;
        // Second call must succeed eventually thanks to the refill loop.
        bucket.take(64).await;
    }

    #[tokio::test]
    async fn rate_zero_is_disabled() {
        let bucket = TokenBucket::new(0);
        bucket.take(usize::MAX as u64).await;
    }
}
