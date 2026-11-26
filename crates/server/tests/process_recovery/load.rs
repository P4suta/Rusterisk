#[cfg(target_os = "linux")]
#[path = "load/linux.rs"]
mod linux;

fn cpu_per_second(start: u64, end: u64, elapsed: std::time::Duration) -> Option<u64> {
    let scaled = u128::from(end.checked_sub(start)?) * 1_000_000_000;
    u64::try_from(scaled.checked_div(elapsed.as_nanos())?).ok()
}

fn two_cpu_profile(quota: &str, status: &str) -> bool {
    let allowed = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed:"));
    let count = allowed.and_then(|mask| {
        mask.trim().split(',').try_fold(0u32, |count, group| {
            count.checked_add(u32::from_str_radix(group, 16).ok()?.count_ones())
        })
    });
    quota.trim() == "200000 100000" && count == Some(2)
}

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
fn a_cpu_sample_uses_elapsed_wall_clock_time() {
    use std::time::Duration;
    assert_eq!(
        cpu_per_second(20, 99_000_020, Duration::from_millis(900)),
        Some(110_000_000)
    );
    assert_eq!(cpu_per_second(9, 3, Duration::from_secs(1)), None);
    assert_eq!(cpu_per_second(0, 0, Duration::ZERO), None);
}

#[test]
fn a_two_cpu_trial_requires_both_the_quota_and_effective_affinity() {
    let status = "Name:\ttest\nCpus_allowed:\t00000001,00000080\nCpus_allowed_list:\t7,32\n";
    assert!(two_cpu_profile("200000 100000\n", status));
    for mask in ["1", "7", "not-a-mask", ""] {
        assert!(!two_cpu_profile(
            "200000 100000",
            &format!("Cpus_allowed:\t{mask}\n")
        ));
    }
    assert!(!two_cpu_profile("max 100000", status));
    assert!(!two_cpu_profile(
        "200000 100000",
        "Cpus_allowed_list:\t7,32\n"
    ));
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
