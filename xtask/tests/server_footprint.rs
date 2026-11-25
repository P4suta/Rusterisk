use std::{fs, io, path::Path, time::Duration};
use xtask::server_distribution::footprint::{first_bidirectional_audio_ms, ProcessFootprint};

fn task(root: &Path, tid: u32, start: u64, voluntary: u64, involuntary: u64) -> io::Result<()> {
    let dir = root.join(format!("42/task/{tid}"));
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("status"), format!("Tgid:\t42\nPid:\t{tid}\nvoluntary_ctxt_switches:\t{voluntary}\nnonvoluntary_ctxt_switches:\t{involuntary}\n"))?;
    fs::write(
        dir.join("stat"),
        format!(
            "{tid} (worker has ) space) S {} {start} 0\n",
            ["0"; 18].join(" ")
        ),
    )?;
    Ok(())
}

#[test]
fn idle_measurement_counts_each_thread_once_and_rejects_replaced_threads() -> io::Result<()> {
    let work = tempfile::tempdir()?;
    let root = work.path();
    task(root, 42, 7, 2, 100)?;
    task(root, 43, 9, 50, 60)?;
    fs::write(root.join("42/status"), "Pid:\t42\nThreads:\t2\n")?;
    fs::write(root.join("42/smaps_rollup"), "Anonymous: 144 kB\n")?;
    fs::write(root.join("42/exe"), [0_u8; 55])?;
    let before = ProcessFootprint::read(root, 42)?;
    task(root, 42, 7, 10, 102)?;
    task(root, 43, 9, 55, 65)?;
    let after = ProcessFootprint::read(root, 42)?;
    let idle = after.since(&before, Duration::from_secs(60))?;
    assert_eq!(after.anonymous_kib, 144);
    assert_eq!(after.executable_bytes, 55);
    assert_eq!(idle.threads.len(), 2);
    assert_eq!(idle.threads[&42].voluntary, 8);
    assert_eq!(idle.threads[&43].involuntary, 5);
    assert_eq!(idle.total(), 20);
    assert!(idle.within_switch_budget(5420));
    task(root, 43, 9, 5500, 65)?;
    assert!(!ProcessFootprint::read(root, 42)?
        .since(&before, Duration::from_secs(60))?
        .within_switch_budget(5420));
    task(root, 43, 10, 55, 65)?;
    assert!(ProcessFootprint::read(root, 42)?
        .since(&before, Duration::from_secs(60))
        .is_err());
    fs::write(root.join("42/smaps_rollup"), "Anonymous: 144 MB\n")?;
    assert!(ProcessFootprint::read(root, 42).is_err());
    Ok(())
}

#[test]
fn first_call_latency_requires_both_received_tones() -> io::Result<()> {
    let record = r#"{"format":"rusterisk-first-call-observations","version":1,"clock":"node-performance-now","unit":"milliseconds","origin":"before-alice-call-click","started":true,"milestones":{"bobOriginateHttpAccepted":8,"bothBrowsersAnswered":20,"aliceObserved660HzTone":32.5,"bobObserved440HzTone":38.25}}"#;
    assert_eq!(first_bidirectional_audio_ms(record)?, 38.25);
    assert!(first_bidirectional_audio_ms(&record.replace("38.25", "null")).is_err());
    assert!(first_bidirectional_audio_ms(&record.replace("32.5", "12")).is_err());
    Ok(())
}
