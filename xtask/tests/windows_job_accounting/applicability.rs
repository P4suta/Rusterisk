use super::fixtures::{assert_rejected, policy, using_fixture, write};
use std::{io, path::Path};
use toml::Value;

fn replace_member(root: &Path, member: &str) -> io::Result<()> {
    let path = root.join("Cargo.toml");
    let mut manifest: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path)?).map_err(io::Error::other)?;
    manifest
        .get_mut("workspace")
        .and_then(Value::as_table_mut)
        .ok_or_else(|| io::Error::other("fixture workspace missing"))?
        .insert("members".into(), Value::Array([member.into()].into()));
    std::fs::write(path, toml::to_string(&manifest).map_err(io::Error::other)?)
}

fn ordinary_member(root: &Path) -> io::Result<()> {
    replace_member(root, "crates/ordinary")?;
    write(
        root,
        "crates/ordinary/src/lib.rs",
        "pub fn ordinary() -> u8 { 1 }\n",
    )
}

#[test]
fn accepts_an_unrelated_workspace_without_query_artifacts_or_registration() -> io::Result<()> {
    let output = using_fixture(|root| {
        ordinary_member(root)?;
        std::fs::remove_dir_all(root.join("crates/windows-job-accounting"))?;
        write(root, "api/policy-exceptions.toml", "exceptions = []\n")?;
        policy(root)
    })?;
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
fn rejects_query_artifacts_when_only_the_explicit_workspace_member_is_removed() -> io::Result<()> {
    let output = using_fixture(|root| {
        ordinary_member(root)?;
        policy(root)
    })?;
    assert_rejected(&output);
    Ok(())
}

#[test]
fn rejects_a_current_directory_alias_for_the_query_member() -> io::Result<()> {
    let output = using_fixture(|root| {
        replace_member(root, "./crates/windows-job-accounting")?;
        policy(root)
    })?;
    assert_rejected(&output);
    Ok(())
}

#[test]
fn rejects_a_parent_directory_alias_for_the_query_member() -> io::Result<()> {
    let output = using_fixture(|root| {
        replace_member(root, "crates/../crates/windows-job-accounting")?;
        policy(root)
    })?;
    assert_rejected(&output);
    Ok(())
}
