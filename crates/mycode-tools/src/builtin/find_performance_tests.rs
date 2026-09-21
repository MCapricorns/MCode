//! Opt-in Phase-1 filesystem-search performance harness.
//!
//! Run with:
//! `cargo test -p mycode-tools --release --locked search_phase1_performance -- --ignored --nocapture`.
//! Each result line reports `p50_ms`, `p95_ms`, `rss_bytes`,
//! `listing_key_allocations`, and `retained_handle_peak`. `rss_bytes` is
//! explicitly unavailable because process-wide RSS cannot isolate one
//! in-crate invocation and this harness does not install a global allocator.
use std::fs::File;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::super::test_support::{text_of, unwrap_tool};
use super::{Limits, compile_find_glob, resolve_search_root_cancel, run_find};

const ENTRY_COUNTS: [usize; 2] = [10_000, 99_000];
const DIRECTORY_COUNT: usize = 100;
const DEFAULT_REPEATS: usize = 5;
const MAX_REPEATS: usize = 20;
/// Keeps slow debug or network-filesystem opt-in runs from hitting the
/// production deadline; the harness remains bounded by its fixed corpora.
const BENCH_TIME_LIMIT: Duration = Duration::from_secs(600);

fn build_corpus(root: &std::path::Path, entries: usize) {
    std::fs::create_dir(root.join(".git")).unwrap();
    let file_count = entries - DIRECTORY_COUNT - 1;
    let files_per_directory = file_count / DIRECTORY_COUNT;
    let remainder = file_count % DIRECTORY_COUNT;
    for directory_index in 0..DIRECTORY_COUNT {
        let directory = root.join(format!("d{directory_index:03}"));
        std::fs::create_dir(&directory).unwrap();
        let count = files_per_directory + usize::from(directory_index < remainder);
        for file_index in 0..count {
            File::create(directory.join(format!("f{file_index:05}.txt"))).unwrap();
        }
    }
}

fn percentile_ms(samples: &[Duration], percentile: usize) -> f64 {
    let rank = samples.len().saturating_mul(percentile).div_ceil(100);
    samples[rank.saturating_sub(1)].as_secs_f64() * 1_000.0
}

/// Manual, threshold-free harness for the bounded 10k and 99k corpora.
#[test]
#[ignore = "manual: cargo test -p mycode-tools --release --locked search_phase1_performance -- --ignored --nocapture"]
fn search_phase1_performance() {
    let repeats = std::env::var("MYCODE_SEARCH_BENCH_REPEATS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_REPEATS)
        .clamp(3, MAX_REPEATS);

    for entry_count in ENTRY_COUNTS {
        let directory = tempfile::tempdir().unwrap();
        build_corpus(directory.path(), entry_count);
        let mut samples = Vec::with_capacity(repeats);
        let mut retained_handle_peak = 0u64;
        let mut listing_key_allocations = 0u64;

        for _ in 0..repeats {
            let limits = Limits {
                time_limit: BENCH_TIME_LIMIT,
                ..Limits::default()
            };
            let cancel = CancellationToken::new();
            let root =
                resolve_search_root_cancel(directory.path(), None, &cancel, &limits).unwrap();
            let limiter = root.limiter.clone();
            let glob = compile_find_glob("*.does-not-exist").unwrap();
            let started = Instant::now();
            let result = unwrap_tool(run_find(glob, root, None, &cancel, &limits));
            samples.push(started.elapsed());

            assert_eq!(text_of(&result), "");
            assert_eq!(result.details.as_ref().unwrap()["matches"], 0);
            assert_eq!(limiter.stopped_reason(), None);
            assert_eq!(
                limiter.listing_key_allocations(),
                entry_count as u64,
                "fixture or listing accounting drifted"
            );
            retained_handle_peak = retained_handle_peak.max(limiter.peak_handles());
            listing_key_allocations = limiter.listing_key_allocations();
        }

        samples.sort_unstable();
        println!(
            "search_phase1 entries={entry_count} repeats={repeats} p50_ms={:.3} p95_ms={:.3} rss_bytes=unavailable listing_key_allocations={listing_key_allocations} retained_handle_peak={retained_handle_peak}",
            percentile_ms(&samples, 50),
            percentile_ms(&samples, 95),
        );
    }
}
