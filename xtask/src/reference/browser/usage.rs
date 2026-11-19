use super::super::commands::{arguments, docker, Commands};
use crate::process::CommandSupervisor;
use std::{
    io::{self, Write},
    num::NonZeroU32,
    path::Path,
    time::{Duration, Instant},
};

fn cpu_usage(source: &str) -> io::Result<u64> {
    let invalid = || io::Error::other("browser server CPU usage counter is invalid");
    let mut rows = source.lines().filter_map(|line| {
        let mut fields = line.split_ascii_whitespace();
        (fields.next() == Some("usage_usec")).then_some(fields)
    });
    let mut fields = rows.next().ok_or_else(invalid)?;
    let value = fields
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid)?;
    if fields.next().is_some() || rows.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}

fn server_child(source: &str) -> io::Result<NonZeroU32> {
    source
        .trim()
        .parse::<NonZeroU32>()
        .ok()
        .filter(|pid| pid.get() > 1)
        .ok_or_else(|| io::Error::other("browser server wrapper has no unique child"))
}

fn status_field<'a>(source: &'a str, field: &str) -> Option<&'a str> {
    let mut values = source.lines().filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == field).then_some(value.trim())
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn process_rss(source: &str, server: &str, pid: NonZeroU32) -> io::Result<u64> {
    if status_field(source, "Name") != Some(server)
        || status_field(source, "Pid").and_then(|value| value.parse().ok()) != Some(pid)
        || status_field(source, "PPid") != Some("1")
    {
        return Err(io::Error::other("browser server child identity differs"));
    }
    let invalid = || io::Error::other("browser server resident memory is invalid");
    let mut fields = status_field(source, "VmRSS")
        .ok_or_else(invalid)?
        .split_ascii_whitespace();
    let kib = fields
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid)?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        return Err(invalid());
    }
    Ok(kib)
}

struct Sample {
    cpu: u64,
    pid: NonZeroU32,
    rss_kib: u64,
}

fn sample(
    id: &str,
    server: &str,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<Sample> {
    let mut read = |path| {
        commands.run(
            docker(&arguments([
                "exec",
                id,
                "/usr/bin/head",
                "--bytes=32768",
                path,
            ])),
            Duration::from_secs(5),
            |command, deadline| supervisor.run(command, deadline),
        )
    };
    let cpu = cpu_usage(read("/sys/fs/cgroup/cpu.stat")?.text()?)?;
    let pid = server_child(read("/proc/1/task/1/children")?.text()?)?;
    let status = format!("/proc/{pid}/status");
    let rss_kib = process_rss(read(&status)?.text()?, server, pid)?;
    Ok(Sample { cpu, pid, rss_kib })
}

pub(super) fn measure<T>(
    id: &str,
    server: &str,
    state: &Path,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
    compare: impl FnOnce(&str) -> io::Result<T>,
) -> io::Result<T> {
    let before = sample(id, server, commands, supervisor)?;
    let started = Instant::now();
    let outcome = compare(id)?;
    let milliseconds = started.elapsed().as_millis();
    let after = sample(id, server, commands, supervisor)?;
    if before.pid != after.pid {
        return Err(io::Error::other(
            "browser server process changed during measurement",
        ));
    }
    let cpu = after
        .cpu
        .checked_sub(before.cpu)
        .ok_or_else(|| io::Error::other("browser server CPU counter moved backwards"))?;
    let mut report = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(state.join(format!("{server}-resources.txt")))?;
    writeln!(
        report,
        "server={server}\ndriver_elapsed_ms={milliseconds}\ncpu_usec={cpu}\nrss_before_kib={}\nrss_after_kib={}",
        before.rss_kib, after.rss_kib
    )?;
    report.flush()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{cpu_usage, process_rss, server_child};
    use std::io;

    #[test]
    fn the_cpu_counter_requires_one_unsigned_usage_value() -> io::Result<()> {
        assert_eq!(
            cpu_usage("usage_usec 72\nuser_usec 35\nsystem_usec 37\n")?,
            72
        );
        for invalid in [
            "",
            "user_usec 72\n",
            "usage_usec -1\n",
            "usage_usec 1 extra\n",
            "usage_usec 1\nusage_usec 2\n",
        ] {
            assert!(cpu_usage(invalid).is_err(), "{invalid:?}");
        }
        Ok(())
    }

    #[test]
    fn a_timeout_wrapper_must_have_one_server_child() -> io::Result<()> {
        assert_eq!(server_child("2 ")?.get(), 2);
        for invalid in ["", "0", "1", "2 3", "2 2", "-2"] {
            assert!(server_child(invalid).is_err(), "{invalid:?}");
        }
        Ok(())
    }

    #[test]
    fn resident_memory_requires_the_expected_child_identity_and_linux_units() -> io::Result<()> {
        let pid = server_child("2 ")?;
        let status = "Name:\trusterisk\nPid:\t2\nPPid:\t1\nVmRSS:\t12864 kB\n";
        assert_eq!(process_rss(status, "rusterisk", pid)?, 12864);
        for invalid in [
            status.replace("Pid:\t2", "Pid:\t3"),
            status.replace("PPid:\t1", "PPid:\t3"),
            status.replace("12864 kB", "13m"),
            status.replace("12864 kB", "-1 kB"),
            status.replace("12864 kB", "12864 kB extra"),
            status.replace("VmRSS", "VmHWM"),
            format!("{status}VmRSS:\t1 kB\n"),
        ] {
            assert!(
                process_rss(&invalid, "rusterisk", pid).is_err(),
                "{invalid:?}"
            );
        }
        assert!(process_rss(status, "asterisk", pid).is_err());
        Ok(())
    }
}
