// 6 concurrent segment workers sharing ONE BudgetChain — total must
// converge to the cap, not cap×6.
use peregrine_api::budget::{BudgetChain, RateBudget};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test]
async fn shared_budget_total_is_the_cap() {
    let chain = Arc::new(BudgetChain {
        local: RateBudget::with_bps(128 * 1024),
        global: RateBudget::unlimited(),
    });
    let taken = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let chain = chain.clone();
        let taken = taken.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..1000 {
                chain.acquire(64 * 1024).await;
                taken.fetch_add(64 * 1024, std::sync::atomic::Ordering::Relaxed);
            }
        }));
    }
    let t0 = Instant::now();
    // Let them run ~4 s then stop.
    tokio::time::sleep(Duration::from_secs(4)).await;
    for h in handles {
        h.abort();
    }
    let bytes = taken.load(std::sync::atomic::Ordering::Relaxed);
    let secs = t0.elapsed().as_secs_f64();
    let rate = bytes as f64 / secs / 1024.0;
    println!("total {bytes} B in {secs:.1}s = {rate:.0} KB/s");
    assert!(
        rate < 160.0,
        "budget leaks: {rate:.0} KB/s > 1.25x cap (128 KB/s)"
    );
}
