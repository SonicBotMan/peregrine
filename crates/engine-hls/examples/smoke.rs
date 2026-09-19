// smoke: cargo run -p peregrine-engine-hls --example smoke -- <url> <out>
use peregrine_api::budget::{BudgetChain, RateBudget};
use peregrine_api::download::{DownloadJob, NoProgress};
use peregrine_engine_hls::HlsEngine;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).unwrap();
    let out = std::env::args().nth(2).unwrap();
    let e = HlsEngine::new().unwrap();
    let job = DownloadJob {
        url,
        sink: out.into(),
        resume: None,
        expected_total: None,
        mirrors: Vec::new(),
        fetch_base: None,
    };
    let r = e
        .download_merge(
            &job,
            Arc::new(NoProgress),
            tokio_util::sync::CancellationToken::new(),
            &BudgetChain {
                local: RateBudget::unlimited(),
                global: RateBudget::unlimited(),
            },
        )
        .await;
    match r {
        Ok(o) => println!("OK {:?} bytes, final={}", o.bytes_written, o.final_url),
        Err(e) => println!("ERR {e}"),
    }
}
