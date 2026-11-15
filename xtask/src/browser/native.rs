use serde::Deserialize;
use std::collections::BTreeSet;

const REQUIRED: [&str; 4] = [
    "actual outgoing and incoming browser calls exchange known audio and save a recording",
    "cancelling or disconnecting during native capture releases late tracks and preserves reconnect",
    "browser controls preserve monochrome reading, keyboard focus, targets, reflow, enlarged text, and spacing",
    "disabled controls explain unavailable JavaScript and a failed application bundle",
];

#[derive(Debug)]
pub struct NativeError;
impl std::fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("browser verification requires every inventoried native test to pass once without skips, retries, errors, or source changes")
    }
}
impl std::error::Error for NativeError {}

#[derive(Deserialize)]
struct Report {
    config: Configuration,
    suites: Vec<Suite>,
    errors: Vec<serde_json::Value>,
    stats: Stats,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Configuration {
    version: String,
    workers: usize,
    forbid_only: bool,
    fully_parallel: bool,
    global_timeout: u64,
    projects: Vec<Project>,
    filtered_projects: Vec<FilteredProject>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    retries: usize,
    repeat_each: usize,
    timeout: u64,
}
#[derive(Deserialize)]
struct FilteredProject {
    #[serde(rename = "use")]
    browser: BrowserUse,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserUse {
    browser_name: String,
    channel: String,
    headless: bool,
    trace: String,
    video: String,
    screenshot: String,
}
#[derive(Deserialize)]
struct Suite {
    #[serde(default)]
    specs: Vec<Spec>,
    #[serde(default)]
    suites: Vec<Suite>,
}
#[derive(Deserialize)]
struct Spec {
    id: String,
    file: String,
    title: String,
    ok: bool,
    tests: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    expected_status: String,
    status: String,
    annotations: Vec<serde_json::Value>,
    results: Vec<Outcome>,
}
#[derive(Deserialize)]
struct Outcome {
    status: String,
    retry: usize,
    errors: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct Stats {
    duration: f64,
    expected: usize,
    skipped: usize,
    unexpected: usize,
    flaky: usize,
}

#[derive(PartialEq, Eq)]
struct Identity {
    id: String,
    file: String,
    title: String,
}
pub struct BrowserInventory {
    cases: Vec<Identity>,
}
impl BrowserInventory {
    pub fn count(&self) -> usize {
        self.cases.len()
    }
}

fn collect(
    suites: Vec<Suite>,
    depth: usize,
    executed: bool,
    cases: &mut Vec<Identity>,
) -> Result<(), NativeError> {
    if depth > 8 || suites.len() > 64 {
        return Err(NativeError);
    }
    for suite in suites {
        for spec in suite.specs {
            if cases.len() >= 64
                || spec.tests.len() != 1
                || !spec.ok
                || spec.id.is_empty()
                || spec.id.len() > 256
                || !spec
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || spec.file.is_empty()
                || spec.file.len() > 256
                || !spec.file.ends_with(".spec.ts")
                || spec.file.split('/').any(|part| {
                    part.is_empty()
                        || matches!(part, "." | "..")
                        || !part
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                })
                || spec.title.is_empty()
                || spec.title.len() > 512
                || spec.title.chars().any(char::is_control)
            {
                return Err(NativeError);
            }
            for case in spec.tests {
                if case.expected_status != "passed" || !case.annotations.is_empty() {
                    return Err(NativeError);
                }
                if executed {
                    if case.status != "expected" || case.results.len() != 1 {
                        return Err(NativeError);
                    }
                    for result in case.results {
                        if result.status != "passed"
                            || result.retry != 0
                            || !result.errors.is_empty()
                        {
                            return Err(NativeError);
                        }
                    }
                } else if case.status != "skipped" || !case.results.is_empty() {
                    return Err(NativeError);
                }
            }
            cases.push(Identity {
                id: spec.id,
                file: spec.file,
                title: spec.title,
            });
        }
        collect(suite.suites, depth + 1, executed, cases)?;
    }
    Ok(())
}

fn parse(bytes: &[u8], executed: bool) -> Result<BrowserInventory, NativeError> {
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        return Err(NativeError);
    }
    let report: Report = serde_json::from_slice(bytes).map_err(|_| NativeError)?;
    if report.config.version != "1.65.1"
        || report.config.workers != 1
        || !report.config.forbid_only
        || report.config.fully_parallel
        || report.config.global_timeout != 120000
        || report.config.projects.len() != 1
        || report.config.filtered_projects.len() != 1
        || !report.errors.is_empty()
    {
        return Err(NativeError);
    }
    for project in report.config.projects {
        if project.retries != 0 || project.repeat_each != 1 || project.timeout != 60000 {
            return Err(NativeError);
        }
    }
    for project in report.config.filtered_projects {
        let browser = project.browser;
        if browser.browser_name != "chromium"
            || browser.channel != "chromium"
            || !browser.headless
            || browser.trace != "off"
            || browser.video != "off"
            || browser.screenshot != "off"
        {
            return Err(NativeError);
        }
    }
    let mut cases = Vec::new();
    collect(report.suites, 0, executed, &mut cases)?;
    if cases.len() < REQUIRED.len() {
        return Err(NativeError);
    }
    let mut identifiers = BTreeSet::new();
    let mut titles = BTreeSet::new();
    for case in &cases {
        if !identifiers.insert(&case.id) || !titles.insert(&case.title) {
            return Err(NativeError);
        }
    }
    if !REQUIRED
        .iter()
        .all(|required| titles.contains(&required.to_string()))
    {
        return Err(NativeError);
    }
    if !report.stats.duration.is_finite()
        || report.stats.duration <= 0.0
        || report.stats.duration > 120000.0
        || report.stats.unexpected != 0
        || report.stats.flaky != 0
    {
        return Err(NativeError);
    }
    if (executed && (report.stats.expected != cases.len() || report.stats.skipped != 0))
        || (!executed && (report.stats.expected != 0 || report.stats.skipped != cases.len()))
    {
        return Err(NativeError);
    }
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(BrowserInventory { cases })
}

pub fn validate_inventory(bytes: &[u8]) -> Result<BrowserInventory, NativeError> {
    parse(bytes, false)
}
pub fn validate_execution(
    bytes: &[u8],
    inventory: &BrowserInventory,
) -> Result<usize, NativeError> {
    let result = parse(bytes, true)?;
    if result.cases != inventory.cases {
        return Err(NativeError);
    }
    Ok(result.count())
}
