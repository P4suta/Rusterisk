use super::AssuranceError;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Test,
    Suite,
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Event {
    Discovery,
    Discovered,
    Started,
    Completed,
    Ok,
    Failed,
    Ignored,
    Timeout,
}
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Kind,
    event: Event,
    name: Option<String>,
    ignore: Option<bool>,
    tests: Option<usize>,
    benchmarks: Option<usize>,
    total: Option<usize>,
    ignored: Option<usize>,
    test_count: Option<usize>,
    passed: Option<usize>,
    failed: Option<usize>,
    measured: Option<usize>,
    filtered_out: Option<usize>,
}

fn records(bytes: &[u8]) -> Result<Vec<Record>, AssuranceError> {
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        return Err(AssuranceError::Results);
    }
    let source = std::str::from_utf8(bytes).map_err(|_| AssuranceError::Results)?;
    let mut records = Vec::new();
    for line in source.lines().filter(|line| !line.is_empty()) {
        if records.len() >= 16384 {
            return Err(AssuranceError::Results);
        }
        records.push(serde_json::from_str(line).map_err(AssuranceError::Json)?);
    }
    Ok(records)
}

pub struct MiriInventory {
    names: BTreeSet<String>,
    suites: usize,
}

pub fn validate_miri_discovery(
    bytes: &[u8],
    expected: &BTreeSet<String>,
    target_count: usize,
) -> Result<MiriInventory, AssuranceError> {
    if expected.is_empty() || target_count == 0 || target_count > 64 {
        return Err(AssuranceError::Inventory);
    }
    let mut names = BTreeSet::new();
    let mut active = None;
    let mut suites = 0;
    for record in records(bytes)? {
        match (record.kind, record.event) {
            (Kind::Suite, Event::Discovery) if active.is_none() => active = Some(0usize),
            (Kind::Test, Event::Discovered) => {
                let count = active.as_mut().ok_or(AssuranceError::Inventory)?;
                let name = record.name.ok_or(AssuranceError::Inventory)?;
                if record.ignore != Some(false) || !expected.contains(&name) || !names.insert(name)
                {
                    return Err(AssuranceError::Inventory);
                }
                *count += 1;
            }
            (Kind::Suite, Event::Completed) => {
                let count = active.take().ok_or(AssuranceError::Inventory)?;
                if count == 0
                    || record.tests != Some(count)
                    || record.total != Some(count)
                    || record.benchmarks != Some(0)
                    || record.ignored != Some(0)
                {
                    return Err(AssuranceError::Inventory);
                }
                suites += 1;
            }
            _ => return Err(AssuranceError::Inventory),
        }
    }
    if active.is_some() || suites != target_count || &names != expected {
        return Err(AssuranceError::Inventory);
    }
    Ok(MiriInventory { names, suites })
}

pub fn validate_miri_execution(
    bytes: &[u8],
    inventory: &MiriInventory,
) -> Result<(), AssuranceError> {
    let mut seen = BTreeSet::new();
    let mut active = None;
    let mut running = None;
    let mut suites = 0;
    for record in records(bytes)? {
        match (record.kind, record.event) {
            (Kind::Suite, Event::Started) if active.is_none() => {
                let count = record
                    .test_count
                    .filter(|count| *count > 0)
                    .ok_or(AssuranceError::Results)?;
                active = Some((count, 0usize));
            }
            (Kind::Test, Event::Started) if active.is_some() && running.is_none() => {
                let name = record.name.ok_or(AssuranceError::Results)?;
                if !inventory.names.contains(&name) || !seen.insert(name.clone()) {
                    return Err(AssuranceError::Results);
                }
                running = Some(name);
            }
            (Kind::Test, Event::Ok) => {
                let name = running.take().ok_or(AssuranceError::Results)?;
                if record.name.as_ref() != Some(&name) {
                    return Err(AssuranceError::Results);
                }
                active.as_mut().ok_or(AssuranceError::Results)?.1 += 1;
            }
            (Kind::Suite, Event::Ok) if running.is_none() => {
                let (declared, completed) = active.take().ok_or(AssuranceError::Results)?;
                if declared != completed
                    || record.passed != Some(completed)
                    || record.failed != Some(0)
                    || record.ignored != Some(0)
                    || record.measured != Some(0)
                    || record.filtered_out != Some(0)
                {
                    return Err(AssuranceError::Results);
                }
                suites += 1;
            }
            _ => return Err(AssuranceError::Results),
        }
    }
    if active.is_some()
        || running.is_some()
        || suites != inventory.suites
        || seen != inventory.names
    {
        return Err(AssuranceError::Results);
    }
    Ok(())
}
