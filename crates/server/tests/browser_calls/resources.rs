use std::{fs::File, io::Read, path::Path};

enum CounterError {
    Unavailable,
    Oversize,
    Invalid,
}

fn number(value: &str) -> Result<u64, CounterError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(CounterError::Invalid);
    }
    value.parse().map_err(|_| CounterError::Invalid)
}

fn read(root: &Path, file: &str, key: Option<&str>) -> Result<u64, CounterError> {
    let mut bytes = Vec::with_capacity(1025);
    File::open(root.join(file))
        .map_err(|_| CounterError::Unavailable)?
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| CounterError::Unavailable)?;
    if bytes.len() > 1024 {
        return Err(CounterError::Oversize);
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| CounterError::Invalid)?;
    let Some(key) = key else {
        return number(source.trim());
    };
    let mut selected = None;
    for line in source.lines() {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() != Some(key) {
            continue;
        }
        if selected.is_some() {
            return Err(CounterError::Invalid);
        }
        selected = Some(number(fields.next().ok_or(CounterError::Invalid)?)?);
        if fields.next().is_some() {
            return Err(CounterError::Invalid);
        }
    }
    selected.ok_or(CounterError::Invalid)
}

pub(super) fn snapshot(root: &Path) -> String {
    [
        ("pids_current", "pids.current", None),
        ("pids_peak", "pids.peak", None),
        ("pids_max", "pids.events", Some("max")),
        ("memory_max", "memory.events", Some("max")),
        ("memory_oom", "memory.events", Some("oom")),
        ("memory_oom_kill", "memory.events", Some("oom_kill")),
    ]
    .into_iter()
    .map(|(name, file, key)| {
        let value = match read(root, file, key) {
            Ok(value) => value.to_string(),
            Err(CounterError::Unavailable) => "unavailable".into(),
            Err(CounterError::Oversize) => "oversize".into(),
            Err(CounterError::Invalid) => "invalid".into(),
        };
        format!("{name}={value}")
    })
    .collect::<Vec<_>>()
    .join(" ")
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[test]
    fn resource_diagnostics_report_only_named_bounded_counters() -> Result<(), std::io::Error> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("pids.current"), "42\n")?;
        fs::write(root.path().join("pids.peak"), "256\n")?;
        fs::write(root.path().join("pids.events"), "max 12\n")?;
        fs::write(
            root.path().join("memory.events"),
            "low 0\nhigh 0\nmax 2\noom 1\noom_kill 1\n",
        )?;
        assert_eq!(
            super::snapshot(root.path()),
            "pids_current=42 pids_peak=256 pids_max=12 memory_max=2 memory_oom=1 memory_oom_kill=1"
        );
        Ok(())
    }

    #[test]
    fn resource_diagnostics_classify_missing_invalid_and_oversize_counters(
    ) -> Result<(), std::io::Error> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("pids.current"), "private input\n")?;
        fs::write(root.path().join("pids.peak"), "9".repeat(1025))?;
        fs::write(root.path().join("pids.events"), "max 1\nmax 2\n")?;
        fs::write(
            root.path().join("memory.events"),
            "oom 18446744073709551616\noom_kill 0\n",
        )?;
        assert_eq!(super::snapshot(root.path()), "pids_current=invalid pids_peak=oversize pids_max=invalid memory_max=invalid memory_oom=invalid memory_oom_kill=0");
        fs::remove_file(root.path().join("pids.current"))?;
        assert!(super::snapshot(root.path()).starts_with("pids_current=unavailable "));
        Ok(())
    }
}
