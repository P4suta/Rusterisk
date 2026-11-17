use std::{
    io,
    path::Path,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};
use toml::Value;

const MEMBER: &str = "crates/windows-job-accounting";
const SOURCE: &str = r#"
#![cfg(windows)]

/// Returns the current number of processes assigned to a borrowed Windows Job handle.
///
/// The count can change immediately after the query.
///
/// # Errors
///
/// Returns the operating-system error when the accounting query fails.
pub fn active_processes(
    handle: ::std::os::windows::io::BorrowedHandle<'_>,
) -> ::std::io::Result<::core::primitive::u32> {
    let mut information =
        ::windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let length = ::core::primitive::u32::try_from(::core::mem::size_of_val(&information))
        .map_err(::std::io::Error::other)?;
    #[expect(unsafe_code, reason = "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md: Read one basic Job accounting record through a borrowed handle.")]
    let queried = unsafe {
        ::windows_sys::Win32::System::JobObjects::QueryInformationJobObject(
            ::std::os::windows::io::AsRawHandle::as_raw_handle(&handle),
            ::windows_sys::Win32::System::JobObjects::JobObjectBasicAccountingInformation,
            ::core::ptr::addr_of_mut!(information).cast(),
            length,
            ::core::ptr::null_mut(),
        )
    };
    if queried == 0 {
        ::core::result::Result::Err(::std::io::Error::last_os_error())
    } else {
        ::core::result::Result::Ok(information.ActiveProcesses)
    }
}
"#;
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
const REGISTRY: &str = r#"
[[exceptions]]
path = "crates/windows-job-accounting/src/lib.rs"
item = "active_processes"
lint = "unsafe_code"
adr = "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md"
rationale = "Read one basic Job accounting record through a borrowed handle."
"#;

pub(super) fn write(root: &Path, relative: &str, contents: &str) -> io::Result<()> {
    let path = root.join(relative);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("fixture parent missing"))?,
    )?;
    std::fs::write(path, contents)
}

fn populate(root: &Path) -> io::Result<()> {
    let mut workspace: toml::Table =
        toml::from_str(include_str!("../../../Cargo.toml")).map_err(io::Error::other)?;
    let settings = workspace
        .get_mut("workspace")
        .and_then(Value::as_table_mut)
        .ok_or_else(|| io::Error::other("fixture workspace missing"))?;
    settings.insert("members".into(), Value::Array([MEMBER.into()].into()));
    let mut lints = settings
        .get("lints")
        .and_then(Value::as_table)
        .cloned()
        .ok_or_else(|| io::Error::other("fixture workspace lints missing"))?;
    lints
        .get_mut("rust")
        .and_then(Value::as_table_mut)
        .ok_or_else(|| io::Error::other("fixture Rust lints missing"))?
        .insert("unsafe_code".into(), "deny".into());
    let mut member: toml::Table = toml::from_str(PROFILE).map_err(io::Error::other)?;
    member.insert("lints".into(), Value::Table(lints));
    write(
        root,
        "Cargo.toml",
        &toml::to_string(&workspace).map_err(io::Error::other)?,
    )?;
    write(
        root,
        "crates/windows-job-accounting/Cargo.toml",
        &toml::to_string(&member).map_err(io::Error::other)?,
    )?;
    write(root, "crates/windows-job-accounting/src/lib.rs", SOURCE)?;
    write(root, "api/policy-exceptions.toml", REGISTRY)?;
    write(
        root,
        "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md",
        "# Query the borrowed Job handle\n",
    )
}

pub(super) fn using_fixture<T>(operation: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-windows-job-accounting-cli-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let result = populate(&root).and_then(|()| operation(&root));
    match (result, std::fs::remove_dir_all(&root)) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
    }
}

pub(super) fn policy(root: &Path) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("policy")
        .env("PATH", "")
        .current_dir(root)
        .output()
}

pub(super) fn assert_rejected(output: &Output) {
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "policy accepted the query substitution"
    );
    assert!(error.contains("windows-job-accounting"), "{error}");
    assert!(!error.contains("panicked"), "{error}");
    assert!(output.stdout.is_empty());
}

#[test]
fn accepts_the_closed_member_root_and_exact_registration_without_external_tools() -> io::Result<()>
{
    let output = using_fixture(policy)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"policy: 1 Rust files inspected\n");
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn rejects_manifest_drift_even_when_the_registered_query_is_unchanged() -> io::Result<()> {
    for (needle, replacement) in [
        ("build = false", "build = true"),
        ("unsafe_code = \"deny\"", "unsafe_code = \"allow\""),
        ("=0.61.2", "0.61"),
    ] {
        let output = using_fixture(|root| {
            let manifest = root.join("crates/windows-job-accounting/Cargo.toml");
            let contents = std::fs::read_to_string(&manifest)?;
            std::fs::write(manifest, contents.replace(needle, replacement))?;
            policy(root)
        })?;
        assert_rejected(&output);
    }
    Ok(())
}

fn unregistered_root(source: &str) -> io::Result<Output> {
    using_fixture(|root| {
        write(root, "api/policy-exceptions.toml", "exceptions = []\n")?;
        write(root, "crates/windows-job-accounting/src/lib.rs", source)?;
        write(
            root,
            "crates/windows-job-accounting/src/query.txt",
            "#[allow(unsafe_code)] pub fn hidden() { unsafe {} }\n",
        )?;
        policy(root)
    })
}

#[test]
fn rejects_missing_query_registration_with_safe_root_source() -> io::Result<()> {
    let output = unregistered_root("#![cfg(windows)]\npub fn active_processes() -> u32 { 0 }\n")?;
    assert_rejected(&output);
    Ok(())
}

#[test]
fn rejects_missing_query_registration_with_indirect_text_source() -> io::Result<()> {
    let output = unregistered_root("#![cfg(windows)]\ninclude!(\"query.txt\");\n")?;
    assert_rejected(&output);
    Ok(())
}

#[test]
fn rejects_a_different_registered_exception_even_when_its_source_is_accepted() -> io::Result<()> {
    let output = using_fixture(|root| {
        let registry = REGISTRY
            .replace("active_processes", "Fixture")
            .replace("unsafe_code", "clippy::large_enum_variant")
            .replace(
                "Read one basic Job accounting record through a borrowed handle.",
                "Keep fixture data inline.",
            );
        write(root, "api/policy-exceptions.toml", &registry)?;
        write(
            root,
            "crates/windows-job-accounting/src/lib.rs",
            "#[expect(clippy::large_enum_variant, reason = \"docs/adr/assign-windows-command-trees-to-jobs-during-creation.md: Keep fixture data inline.\")] enum Fixture { Empty }\n",
        )?;
        policy(root)
    })?;
    assert_rejected(&output);
    Ok(())
}
