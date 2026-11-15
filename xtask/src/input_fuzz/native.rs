use super::{FuzzError, Target};

pub fn validate_inventory(bytes: &[u8]) -> Result<(), FuzzError> {
    let text = std::str::from_utf8(bytes).map_err(|_| FuzzError::Inventory)?;
    let mut names: Vec<_> = text.lines().filter(|line| !line.is_empty()).collect();
    names.sort_unstable();
    let mut expected = Target::ALL.map(Target::name);
    expected.sort_unstable();
    if names != expected {
        return Err(FuzzError::Inventory);
    }
    Ok(())
}

pub fn validate_run(bytes: &[u8], seeds: usize) -> Result<u64, FuzzError> {
    let text = std::str::from_utf8(bytes).map_err(|_| FuzzError::Results)?;
    if seeds == 0
        || text.lines().any(|line| {
            line.contains("ERROR:")
                || line.contains("libFuzzer: timeout")
                || line.contains("SUMMARY:")
        })
    {
        return Err(FuzzError::Results);
    }
    let mut done = None;
    let mut units = None;
    let mut coverage = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Done ") {
            let (count, elapsed) = rest.split_once(" runs in ").ok_or(FuzzError::Results)?;
            let seconds = elapsed
                .strip_suffix(" second(s)")
                .ok_or(FuzzError::Results)?
                .parse::<u64>()
                .map_err(|_| FuzzError::Results)?;
            let count = count.parse::<u64>().map_err(|_| FuzzError::Results)?;
            if seconds > 70 || done.replace(count).is_some() {
                return Err(FuzzError::Results);
            }
        } else if let Some(rest) = line.strip_prefix("stat::number_of_executed_units:") {
            let count = rest.trim().parse::<u64>().map_err(|_| FuzzError::Results)?;
            if units.replace(count).is_some() {
                return Err(FuzzError::Results);
            }
        } else if line.starts_with('#') && line.split_ascii_whitespace().nth(1) == Some("DONE") {
            let (count, statistics) = line[1..]
                .split_once(|character: char| character.is_ascii_whitespace())
                .ok_or(FuzzError::Results)?;
            let count = count.parse::<u64>().map_err(|_| FuzzError::Results)?;
            let (_, rest) = statistics.split_once("cov: ").ok_or(FuzzError::Results)?;
            let value = rest
                .split_whitespace()
                .next()
                .ok_or(FuzzError::Results)?
                .parse::<u64>()
                .map_err(|_| FuzzError::Results)?;
            if value == 0 || coverage.replace(count).is_some() {
                return Err(FuzzError::Results);
            }
        }
    }
    let count = done.ok_or(FuzzError::Results)?;
    if count < seeds as u64 || count > 100000 || units != Some(count) || coverage != Some(count) {
        return Err(FuzzError::Results);
    }
    Ok(count)
}
