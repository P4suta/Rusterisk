use crate::Finding;
use toml::Value;

const MEMBER: &str = "crates/windows-job-accounting";
const PROFILE: &str = r#"
[package]
name = "rusterisk-windows-job-accounting"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false
build = false
autolib = false
autobins = false
autoexamples = false
autotests = false
autobenches = false

[lib]
path = "src/lib.rs"
crate-type = ["rlib"]
proc-macro = false
test = false
bench = false
doctest = false

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "=0.61.2", default-features = false, features = ["Win32_Foundation", "Win32_System_JobObjects"] }
"#;

fn reject(findings: &mut Vec<Finding>, detail: &str) {
    findings.push(Finding {
        rule: "windows-job-accounting-manifest",
        detail: detail.into(),
    });
}

/// Checks that the closed query manifest matches the workspace lint configuration.
///
/// # Errors
///
/// Returns the parsing error for malformed input.
pub fn inspect_windows_job_accounting_manifest(
    workspace_source: &str,
    member_source: &str,
) -> Result<Vec<Finding>, toml::de::Error> {
    let workspace: toml::Table = toml::from_str(workspace_source)?;
    let member: toml::Table = toml::from_str(member_source)?;
    let mut findings = Vec::new();
    let workspace = workspace.get("workspace").and_then(Value::as_table);
    let members = workspace
        .and_then(|value| value.get("members"))
        .and_then(Value::as_array);
    if members.map(|values| {
        values
            .iter()
            .filter(|value| value.as_str() == Some(MEMBER))
            .count()
    }) != Some(1)
    {
        reject(
            &mut findings,
            "the query crate must be one explicit workspace member",
        );
    }
    let Some(mut lints) = workspace
        .and_then(|value| value.get("lints"))
        .and_then(Value::as_table)
        .cloned()
    else {
        reject(&mut findings, "workspace lint configuration is required");
        return Ok(findings);
    };
    let Some(rust) = lints.get_mut("rust").and_then(Value::as_table_mut) else {
        reject(
            &mut findings,
            "workspace Rust lint configuration is required",
        );
        return Ok(findings);
    };
    if rust.get("unsafe_code").and_then(Value::as_str) != Some("forbid") {
        reject(&mut findings, "workspace unsafe_code must remain forbid");
    }
    rust.insert("unsafe_code".into(), Value::String("deny".into()));
    let mut expected: toml::Table = toml::from_str(PROFILE)?;
    expected.insert("lints".into(), Value::Table(lints));
    if member != expected {
        reject(
            &mut findings,
            "the query manifest must match the closed profile and exact workspace lint mirror",
        );
    }
    Ok(findings)
}

/// Checks that the complete query root has exactly one matching registration.
///
/// # Errors
///
/// Returns the parsing error for malformed source.
pub fn inspect_windows_job_accounting_source(
    source: &str,
    exceptions: &[crate::Exception],
) -> Result<Vec<Finding>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut findings = Vec::new();
    if exceptions.len() != 1 || crate::unsafe_policy::accounting_query(&file, exceptions)?.is_none()
    {
        findings.push(Finding {
            rule: "windows-job-accounting-source",
            detail: "the query root and its single registration must match the closed profile"
                .into(),
        });
    }
    Ok(findings)
}
