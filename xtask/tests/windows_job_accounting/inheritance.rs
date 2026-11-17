use super::{
    applicability::{ordinary_package, replace_member},
    fixtures::{policy, using_fixture, write},
};
use std::{io, path::Path, process::Output};
use toml::Value;

fn add_ordinary_member(root: &Path) -> io::Result<()> {
    ordinary_package(root)?;
    let path = root.join("Cargo.toml");
    let mut manifest: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path)?).map_err(io::Error::other)?;
    manifest
        .get_mut("workspace")
        .and_then(Value::as_table_mut)
        .and_then(|workspace| workspace.get_mut("members"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| io::Error::other("fixture members missing"))?
        .push("crates/ordinary".into());
    std::fs::write(path, toml::to_string(&manifest).map_err(io::Error::other)?)
}

fn hidden_source(root: &Path) -> io::Result<()> {
    write(
        root,
        "crates/ordinary/src/lib.rs",
        "include!(\"query.txt\");\n",
    )?;
    write(
        root,
        "crates/ordinary/src/query.txt",
        "#[allow(unsafe_code)] pub fn hidden() { unsafe {} }\n",
    )
}

fn ordinary_lints_output(lints: &str) -> io::Result<Output> {
    using_fixture(|root| {
        add_ordinary_member(root)?;
        hidden_source(root)?;
        let path = root.join("crates/ordinary/Cargo.toml");
        let mut manifest: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path)?).map_err(io::Error::other)?;
        if lints.is_empty() {
            manifest
                .remove("lints")
                .ok_or_else(|| io::Error::other("fixture lints missing"))?;
        } else {
            let lints: toml::Table = toml::from_str(lints).map_err(io::Error::other)?;
            manifest.insert("lints".into(), Value::Table(lints));
        }
        std::fs::write(path, toml::to_string(&manifest).map_err(io::Error::other)?)?;
        policy(root)
    })
}

fn root_lints_output(level: &str) -> io::Result<Output> {
    using_fixture(|root| {
        ordinary_package(root)?;
        replace_member(root, "crates/ordinary")?;
        hidden_source(root)?;
        std::fs::remove_dir_all(root.join("crates/windows-job-accounting"))?;
        write(root, "api/policy-exceptions.toml", "exceptions = []\n")?;
        let path = root.join("Cargo.toml");
        let mut manifest: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path)?).map_err(io::Error::other)?;
        manifest
            .get_mut("workspace")
            .and_then(Value::as_table_mut)
            .and_then(|workspace| workspace.get_mut("lints"))
            .and_then(Value::as_table_mut)
            .and_then(|lints| lints.get_mut("rust"))
            .and_then(Value::as_table_mut)
            .ok_or_else(|| io::Error::other("fixture workspace Rust lints missing"))?
            .insert("unsafe_code".into(), level.into());
        std::fs::write(path, toml::to_string(&manifest).map_err(io::Error::other)?)?;
        policy(root)
    })
}

fn assert_rejected(output: &Output, path: &str) {
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "policy accepted the compiler lint substitution"
    );
    assert!(error.contains(path), "{error}");
    assert!(!error.contains("panicked"), "{error}");
    assert!(output.stdout.is_empty());
}

#[test]
fn accepts_the_query_leaf_and_an_ordinary_member_inheriting_workspace_lints() -> io::Result<()> {
    let output = using_fixture(|root| {
        add_ordinary_member(root)?;
        policy(root)
    })?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"policy: 2 Rust files inspected\n");
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn rejects_a_member_without_inherited_lints_and_with_unsafe_hidden_in_text() -> io::Result<()> {
    let output = ordinary_lints_output("")?;
    assert_rejected(&output, "crates/ordinary/Cargo.toml");
    Ok(())
}

#[test]
fn rejects_a_member_allow_override_with_unsafe_hidden_in_text() -> io::Result<()> {
    let output = ordinary_lints_output("[rust]\nunsafe_code = \"allow\"\n")?;
    assert_rejected(&output, "crates/ordinary/Cargo.toml");
    Ok(())
}

#[test]
fn rejects_a_member_deny_override_with_unsafe_hidden_in_text() -> io::Result<()> {
    let output = ordinary_lints_output("[rust]\nunsafe_code = \"deny\"\n")?;
    assert_rejected(&output, "crates/ordinary/Cargo.toml");
    Ok(())
}

#[test]
fn rejects_root_allow_without_any_query_artifacts() -> io::Result<()> {
    let output = root_lints_output("allow")?;
    assert_rejected(&output, "Cargo.toml");
    Ok(())
}

#[test]
fn rejects_root_deny_without_any_query_artifacts() -> io::Result<()> {
    let output = root_lints_output("deny")?;
    assert_rejected(&output, "Cargo.toml");
    Ok(())
}
