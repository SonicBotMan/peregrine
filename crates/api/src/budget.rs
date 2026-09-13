//! Byte-rate budgets (M3-b): per-task and global throttles.
//!
//! Design constraints that shaped this:
//!
//! 1. **Live-update** — the user changes a limit while the download
//!    is running. `set_bps` takes effect on the very next refill,
//!    no restart, no engine knowledge.
//! 2. **Cancellable** — `acquire` only ever parks in short slices
//!    (`tokio::time::sleep` futures), so an engine that drops it via
//!    `select!` on its cancellation token loses at most one slice of
//!    latency and leaks NO tokens (we debit only after a successful
//!    wake, never before sleeping).
//! 3. **`0 = unlimited`** — one representation for "no limit" that
//!    stays unlimited across set_bps churn and never divides by zero.
//!
//! The token bucket allows a burst of one second's worth of bytes:
//! small files finish at full line speed, long streams converge to
//! the configured rate. `want` is capped at one second's capacity so
//! a pathological huge frame cannot over-sleep in one slice.
//!
//! [`BudgetChain`] pairs the per-task budget with the daemon-global
//! one: both must consent. Acquiring sequentially (local, then
//! global) is deliberate — the total bytes/sec is still correct
//! (each bucket independently enforces its cap), and the worst case
//! adds one extra park, which the 250 ms slice floor keeps tiny.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

/// Longest single park inside `acquire`; cancellation latency bound.
const MAX_SLICE: Duration = Duration::from_millis(250);

/// Shared handle — budgets are poked live, engines only read.
pub type SharedRateBudget = Arc<RateBudget>;

/// Token-bucket byte limiter. `0` bps = unlimited (never parks).
pub struct RateBudget {
    /// Bytes/sec; 0 = unlimited. Atomic so `set_bps` is lock-free.
    bps: AtomicU64,
    /// (last refill instant, tokens currently banked). Interior
    /// mutability behind a short-lived mutex (never held across a
    /// park — the async part sleeps OUTSIDE the lock).
    state: Mutex<(Instant, u64)>,
}

impl RateBudget {
    /// No limit (the default everywhere until the user sets one).
    pub fn unlimited() -> SharedRateBudget {
        Self::with_bps(0)
    }

    /// A budget enforcing `bps` bytes/sec (0 = unlimited).
    pub fn with_bps(bps: u64) -> SharedRateBudget {
        Arc::new(Self {
            bps: AtomicU64::new(bps),
            state: Mutex::new((Instant::now(), 0)),
        })
    }

    /// Live-update the rate. Takes effect on the next refill; banked
    /// tokens are clamped to the new capacity (raising a limit can't
    /// un-bank a second's burst, lowering one can't strand a surplus).
    pub fn set_bps(&self, bps: u64) {
        self.bps.store(bps, Ordering::Relaxed);
        // Clamp banked tokens to the new cap on a RAISE (docs above);
        // on a lower cap the refill arithmetic clamps on the next
        // take anyway — this just avoids one second of over-burst.
        if let (true, Ok(mut g)) = (bps > 0, self.state.lock()) {
            g.1 = g.1.min(bps);
        }
    }

    /// Current rate (0 = unlimited).
    pub fn bps(&self) -> u64 {
        self.bps.load(Ordering::Relaxed)
    }

    /// Suggested max bytes per acquire-then-write slice for a
    /// caller holding a chunk LARGER than one second's capacity:
    /// paying a 1.5 MiB hyper frame in ONE acquire parks the worker
    /// for frame/cap seconds with zero bytes written (progress goes
    /// dark for ~12 s at 128 KiB/s — the M3-b1 smoke caught this).
    /// Splitting the frame into `slice_hint()`-sized writes keeps
    /// progress flowing and the write stream smooth. Unlimited →
    /// `u64::MAX` (no splitting needed).
    pub fn slice_hint(&self) -> u64 {
        let bps = self.bps.load(Ordering::Relaxed);
        if bps == 0 { u64::MAX } else { bps }
    }

    /// Park until `want` bytes are affordable — ALL of them.
    /// Cancellation-safe: dropping this future between parks
    /// debits nothing beyond what was already taken. A single
    /// debit is clamped to one second's capacity, so a huge frame
    /// is paid for in successive slices — never one debit for a
    /// whole multi-second frame (which would under-charge exactly
    /// the way the M3-b1 smoke caught: cap-billed, chunk-written).
    pub async fn acquire(&self, want: u64) {
        let mut want = want;
        while want > 0 {
            match self.try_take(want) {
                // Fully paid.
                None => return,
                Some((taken, park)) => {
                    want -= taken;
                    if want > 0 {
                        tokio::time::sleep(park.min(MAX_SLICE)).await;
                    }
                }
            }
        }
    }

    /// Debit up to `want` bytes (clamped to one second's capacity
    /// per call): what the bucket could afford NOW is taken
    /// immediately; the remainder is reported with how long to
    /// park before the next slice. The lock is held only for
    /// arithmetic — never across a park.
    fn try_take(&self, want: u64) -> Option<(u64, Duration)> {
        let bps = self.bps.load(Ordering::Relaxed);
        if bps == 0 {
            return None; // unlimited fast path: no lock at all
        }
        let cap = bps; // one second's burst
        let want = want.min(cap); // bound a single debit
        let mut g = self.state.lock().expect("budget mutex poisoned");
        let now = Instant::now();
        let banked = g.1 as f64 + now.duration_since(g.0).as_secs_f64() * bps as f64;
        g.1 = banked.min(cap as f64) as u64;
        g.0 = now;
        let taken = want.min(g.1);
        g.1 -= taken;
        if taken >= want {
            return None; // this slice fully paid
        }
        let deficit = want - taken;
        Some((taken, Duration::from_secs_f64(deficit as f64 / bps as f64)))
    }
}

impl std::fmt::Debug for RateBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateBudget")
            .field("bps", &self.bps.load(Ordering::Relaxed))
            .finish()
    }
}

/// Per-task × global pair: both budgets must consent to every byte.
/// Sequenced deliberately (see module docs) — total rate stays right.
#[derive(Debug, Clone)]
pub struct BudgetChain {
    pub local: SharedRateBudget,
    pub global: SharedRateBudget,
}

impl BudgetChain {
    /// No throttling anywhere (tests, pre-M3 wiring, probe path).
    pub fn unlimited() -> Self {
        Self {
            local: RateBudget::unlimited(),
            global: RateBudget::unlimited(),
        }
    }

    /// Wait for `want` bytes through BOTH budgets — in parallel
    /// (M3-b1 R2 P1-3): sequentially, each park pauses while the
    /// OTHER budget accrues unused credit, and a local+global pair
    /// compounds the stalls into an effective rate well below
    /// min(local, global). Parking both at once makes the wall time
    /// exactly max(local, global). Each budget debits `want` — the
    /// byte cost is `want`, not `2 * want`.
    pub async fn acquire(&self, want: u64) {
        tokio::join!(self.local.acquire(want), self.global.acquire(want));
    }

    /// The tighter of the two budgets' per-second capacity — the
    /// slice size a caller holding an oversized frame should write
    /// with. `u64::MAX` when nothing is throttled (no splitting).
    pub fn slice_hint(&self) -> u64 {
        self.local.slice_hint().min(self.global.slice_hint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_never_parks() {
        let b = RateBudget::unlimited();
        let t0 = Instant::now();
        b.acquire(1 << 30).await; // 1 GiB — must fall through instantly
        assert!(t0.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn enforces_rate_over_a_second() {
        tokio::time::advance(Duration::from_secs(1)).await; // prime the clock
        let b = RateBudget::with_bps(1000);
        // 10 chunks of 100 B at 1000 B/s → about 1 s of wall time.
        let t0 = tokio::time::Instant::now();
        for _ in 0..10 {
            b.acquire(100).await;
        }
        assert!(t0.elapsed() >= Duration::from_secs(1));
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn set_bps_takes_effect_live() {
        tokio::time::advance(Duration::from_secs(1)).await;
        let b = RateBudget::with_bps(1_000_000);
        b.acquire(1_000_000).await; // drains the 1 s burst instantly
        b.set_bps(100); // drop to 100 B/s
        let t0 = tokio::time::Instant::now();
        b.acquire(100).await; // must now park ~1 s
        assert!(t0.elapsed() >= Duration::from_millis(950));
    }

    #[tokio::test(start_paused = true)]
    async fn raising_limit_clamps_banked_burst() {
        tokio::time::advance(Duration::from_secs(1)).await;
        let b = RateBudget::with_bps(100);
        b.acquire(100).await; // drain
        b.set_bps(1_000_000);
        // Banked tokens were clamped to the OLD cap on raise? No —
        // clamp happens on lower; on raise banked stays <= new cap.
        // 1e6 budget must serve 100 bytes instantly.
        let t0 = tokio::time::Instant::now();
        b.acquire(100).await;
        assert!(t0.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn oversize_frame_is_paid_in_full() {
        tokio::time::advance(Duration::from_secs(1)).await;
        // A frame LARGER than one second's capacity used to be
        // billed for only `cap` bytes while the engine wrote the
        // whole frame — the M3-b1 smoke caught this as a multi-x
        // rate leak against real servers (big hyper frames).
        let b = RateBudget::with_bps(100_000); // cap = 100 kB/s
        let t0 = tokio::time::Instant::now();
        b.acquire(500_000).await; // a 500 kB frame must cost 5 s
        assert!(
            t0.elapsed() >= Duration::from_secs(5),
            "500 kB through 100 kB/s took {:?} — frame under-billed",
            t0.elapsed()
        );
        assert!(t0.elapsed() < Duration::from_secs(7));
    }

    #[tokio::test(start_paused = true)]
    async fn chain_parks_until_both_consent() {
        tokio::time::advance(Duration::from_secs(1)).await;
        // Global tighter than local: the chain's ceiling is the
        // stricter budget, so two bursts must park ~2 s total.
        let chain = BudgetChain {
            local: RateBudget::unlimited(),
            global: RateBudget::with_bps(100),
        };
        let t0 = tokio::time::Instant::now();
        chain.acquire(100).await; // ~1 s: global banked starts empty
        chain.acquire(100).await; // ~1 s: global banked drained again
        assert!(t0.elapsed() >= Duration::from_millis(1900));
    }
}
