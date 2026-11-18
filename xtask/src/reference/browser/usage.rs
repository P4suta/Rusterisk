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

struct ProcessRss {
    pid: NonZeroU32,
    kib: u64,
}

fn process_rss(source: &str, server: &str) -> io::Result<ProcessRss> {
    let invalid = || io::Error::other("browser server process or resident memory is ambiguous");
    let mut rows = source.lines().filter_map(|line| {
        let mut fields = line.split_ascii_whitespace();
        let pid = fields.next()?;
        (fields.next() == Some(server)).then_some((pid, fields))
    });
    let (pid, mut fields) = rows.next().ok_or_else(invalid)?;
    let pid = pid.parse().map_err(|_| invalid())?;
    let kib = fields
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid)?;
    if fields.next().is_some() || rows.next().is_some() {
        return Err(invalid());
    }
    Ok(ProcessRss { pid, kib })
}

struct Sample {
    cpu: u64,
    process: ProcessRss,
}

fn sample(
    id: &str,
    server: &str,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<Sample> {
    let budget = Duration::from_secs(5);
    let stat = commands.run(
        docker(&arguments([
            "exec",
            id,
            "/usr/bin/head",
            "--bytes=32768",
            "/sys/fs/cgroup/cpu.stat",
        ])),
        budget,
        |command, deadline| supervisor.run(command, deadline),
    )?;
    let top = commands.run(
        docker(&arguments(["top", id, "-eo", "pid,comm,rss"])),
        budget,
        |command, deadline| supervisor.run(command, deadline),
    )?;
    Ok(Sample {
        cpu: cpu_usage(stat.text()?)?,
        process: process_rss(top.text()?, server)?,
    })
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
    if before.process.pid != after.process.pid {
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
        before.process.kib, after.process.kib
    )?;
    report.flush()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{cpu_usage, process_rss};
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
    fn resident_memory_requires_one_identified_server_process() -> io::Result<()> {
        let top = "PID COMMAND RSS\n2 timeout 144\n100 rusterisk 12864\n";
        let process = process_rss(top, "rusterisk")?;
        assert_eq!(process.pid.get(), 100);
        assert_eq!(process.kib, 12864);
        for invalid in [
            "PID COMMAND RSS\n2 timeout 144\n",
            "PID COMMAND RSS\n0 rusterisk 12864\n",
            "PID COMMAND RSS\n100 rusterisk -1\n",
            "PID COMMAND RSS\n100 rusterisk 1\n101 rusterisk 2\n",
            "PID COMMAND RSS\n100 rusterisk-worker 1\n",
        ] {
            assert!(process_rss(invalid, "rusterisk").is_err(), "{invalid:?}");
        }
        assert!(process_rss(top, "asterisk").is_err());
        Ok(())
    }
}
