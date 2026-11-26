#[cfg(target_os = "linux")]
#[path = "load/linux.rs"]
mod linux;

fn within_baseline(mut baseline: [u64; 5], mut candidate: [u64; 5]) -> bool {
    baseline.sort_unstable();
    candidate.sort_unstable();
    candidate[2] <= baseline[4]
}

fn within_arm64_rss(legs: u8, mut kib: [u64; 5]) -> bool {
    let limit = match legs {
        0 => 14_848,
        2 => 15_872,
        16 => 16_384,
        64 => 18_432,
        _ => return false,
    };
    kib.sort_unstable();
    kib[2] <= limit
}

#[test]
fn the_candidate_median_cannot_exceed_any_baseline_limit() {
    assert!(within_baseline([3, 1, 5, 4, 2], [80, 5, 2, 98, 1]));
    assert!(!within_baseline([3, 1, 5, 4, 2], [80, 6, 2, 98, 1]));
}

#[test]
fn arm64_memory_limits_keep_the_measured_savings_at_each_load() {
    for (legs, kib) in [(0, 14_848), (2, 15_872), (16, 16_384), (64, 18_432)] {
        assert!(within_arm64_rss(legs, [kib + 1, kib, kib, kib, kib + 1]));
        assert!(!within_arm64_rss(
            legs,
            [kib, kib, kib + 1, kib + 1, kib + 1]
        ));
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "Requires two packaged servers in a two-CPU Linux container."]
async fn compare_five_alternating_trials_at_each_call_load(
) -> Result<(), rusterisk_server::ControlError> {
    linux::compare().await
}
