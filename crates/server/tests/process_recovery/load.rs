#[cfg(target_os = "linux")]
#[path = "load/linux.rs"]
mod linux;

use super::{combine, failure, ChildServer, ControlError};
use std::path::Path;

fn finish_trial<T>(
    server: &mut ChildServer,
    root: &Path,
    result: Result<T, ControlError>,
) -> Result<T, ControlError> {
    if result.is_err() {
        match rusterisk_test_support::child_exit(
            &mut server.child,
            rusterisk_test_support::Wait::Responsive,
        ) {
            Ok(status) => eprintln!("Server exited during the trial: {status}"),
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                eprintln!("Server still running after the failed request")
            }
            Err(error) => eprintln!("Failed to read the trial process status: {error}"),
        }
        for name in ["server.0.log", "server.0.stderr"] {
            match std::fs::read_to_string(root.join(name)) {
                Ok(messages) => {
                    for line in messages.lines().rev().take(5) {
                        eprintln!("{line}");
                    }
                }
                Err(error) => eprintln!("Failed to read {name}: {error}"),
            }
        }
    }
    combine(result, server.terminate().map(|_| ()))
}

#[tokio::test]
async fn a_missing_trial_log_preserves_the_failure_and_reaps_the_server() -> Result<(), ControlError>
{
    let root = tempfile::tempdir().map_err(failure)?;
    std::fs::write(
        root.path().join("ari.conf"),
        "[app]\ntype=user\npassword=test-password\n",
    )
    .map_err(failure)?;
    for file in ["pjsip.conf", "rtp.conf", "extensions.conf"] {
        std::fs::write(root.path().join(file), "").map_err(failure)?;
    }
    let mut server = ChildServer::start(
        root.path(),
        &root.path().join("data"),
        root.path().join("server.log"),
    )
    .await?;
    let result = finish_trial(
        &mut server,
        &root.path().join("missing"),
        Err::<(), _>(ControlError::Invalid("measurement failed")),
    );
    let reaped = server.child.try_wait().map_err(failure)?.is_some();
    server.terminate()?;
    assert!(matches!(
        result,
        Err(ControlError::Invalid("measurement failed"))
    ));
    assert!(reaped);
    root.close().map_err(failure)
}

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
