const WORKSPACE: &str = r#"
[workspace]
members = ["xtask", "crates/windows-job-accounting"]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.99"
license = "MIT OR Apache-2.0"

[workspace.lints.rust]
unsafe_code = "forbid"
unused_must_use = "deny"
unfulfilled_lint_expectations = "deny"
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(kani)'] }

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
await_holding_lock = "deny"
clone_on_ref_ptr = "deny"
disallowed_types = "deny"
exit = "deny"
let_underscore_future = "deny"
mem_forget = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
unwrap_used = "deny"
expect_used = "deny"
wildcard_enum_match_arm = "deny"
"#;

const MEMBER: &str = r#"
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

[lints.rust]
unsafe_code = "deny"
unused_must_use = "deny"
unfulfilled_lint_expectations = "deny"
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(kani)'] }

[lints.clippy]
all = { level = "deny", priority = -1 }
await_holding_lock = "deny"
clone_on_ref_ptr = "deny"
disallowed_types = "deny"
exit = "deny"
let_underscore_future = "deny"
mem_forget = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
unwrap_used = "deny"
expect_used = "deny"
wildcard_enum_match_arm = "deny"
"#;

fn assert_rejected(workspace: &str, member: &str) -> Result<(), toml::de::Error> {
    let findings = xtask::inspect_windows_job_accounting_manifest(workspace, member)?;
    assert!(!findings.is_empty(), "{workspace}\n{member}");
    Ok(())
}

#[test]
fn accepts_only_the_private_library_and_exact_workspace_lint_mirror() -> Result<(), toml::de::Error>
{
    assert!(xtask::inspect_windows_job_accounting_manifest(WORKSPACE, MEMBER)?.is_empty());
    for (needle, replacement) in [
        (
            "unused_must_use = \"deny\"",
            "unused_must_use = \"deny\"\nunsafe_op_in_unsafe_fn = \"deny\"",
        ),
        (
            "disallowed_types = \"deny\"",
            "disallowed_types = \"deny\"\ndisallowed_methods = \"deny\"",
        ),
    ] {
        assert!(xtask::inspect_windows_job_accounting_manifest(
            &WORKSPACE.replace(needle, replacement),
            &MEMBER.replace(needle, replacement)
        )?
        .is_empty());
    }
    Ok(())
}

#[test]
fn rejects_build_hooks_automatic_targets_and_procedural_macro_mode() -> Result<(), toml::de::Error>
{
    for (needle, replacement) in [
        (
            "name = \"rusterisk-windows-job-accounting\"",
            "name = \"other\"",
        ),
        ("\nversion.workspace = true\n", "\nversion = \"0.1.0\"\n"),
        ("edition.workspace = true", "edition = \"2021\""),
        ("rust-version.workspace = true", "rust-version = \"1.99\""),
        (
            "license.workspace = true",
            "license = \"MIT OR Apache-2.0\"",
        ),
        ("build = false", "build = true"),
        ("build = false", "build = \"generate.rs\""),
        ("build = false\n", ""),
        ("autolib = false", "autolib = true"),
        ("autobins = false", "autobins = true"),
        ("autoexamples = false", "autoexamples = true"),
        ("autotests = false", "autotests = true"),
        ("autobenches = false", "autobenches = true"),
        ("proc-macro = false", "proc-macro = true"),
        ("path = \"src/lib.rs\"", "path = \"generated.rs\""),
        ("crate-type = [\"rlib\"]", "crate-type = [\"cdylib\"]"),
        ("\ntest = false\n", "\ntest = true\n"),
        ("bench = false", "bench = true"),
        ("doctest = false", "doctest = true"),
        ("publish = false", "publish = true"),
    ] {
        assert_rejected(WORKSPACE, &MEMBER.replace(needle, replacement))?;
    }
    for extra in [
        "\n[[bin]]\nname = \"extra\"\npath = \"extra.rs\"\n",
        "\n[[test]]\nname = \"extra\"\npath = \"extra.rs\"\n",
        "\n[[example]]\nname = \"extra\"\npath = \"extra.rs\"\n",
        "\n[[bench]]\nname = \"extra\"\npath = \"extra.rs\"\n",
        "\n[build-dependencies]\nextra = \"=1.0.0\"\n",
        "\n[dev-dependencies]\nextra = \"=1.0.0\"\n",
        "\n[features]\nextra = []\n",
    ] {
        assert_rejected(WORKSPACE, &format!("{MEMBER}{extra}"))?;
    }
    Ok(())
}

#[test]
fn rejects_dependency_version_features_aliases_and_platform_scope_drift(
) -> Result<(), toml::de::Error> {
    for (needle, replacement) in [
        ("=0.61.2", "0.61"),
        ("=0.61.2", "=0.61.1"),
        ("default-features = false", "default-features = true"),
        ("\"Win32_Foundation\", ", ""),
        ("\"Win32_System_JobObjects\"", "\"Win32_System_Threading\""),
        ("windows-sys =", "alias ="),
        (
            "version = \"=0.61.2\",",
            "version = \"=0.61.2\", path = \"../other\",",
        ),
        (
            "version = \"=0.61.2\",",
            "version = \"=0.61.2\", package = \"other\",",
        ),
        ("cfg(windows)", "cfg(unix)"),
        ("[target.'cfg(windows)'.dependencies]", "[dependencies]"),
    ] {
        assert_rejected(WORKSPACE, &MEMBER.replace(needle, replacement))?;
    }
    let dependency = "windows-sys = { version = \"=0.61.2\", default-features = false, features = [\"Win32_Foundation\", \"Win32_System_JobObjects\"] }";
    assert_rejected(WORKSPACE, &MEMBER.replace(dependency, ""))?;
    assert_rejected(
        WORKSPACE,
        &MEMBER.replace(dependency, &format!("{dependency}\nextra = \"=1.0.0\"")),
    )?;
    Ok(())
}

#[test]
fn malformed_workspace_or_member_toml_never_passes() {
    assert!(xtask::inspect_windows_job_accounting_manifest("[", MEMBER).is_err());
    assert!(xtask::inspect_windows_job_accounting_manifest(WORKSPACE, "[").is_err());
}

#[test]
fn rejects_every_lint_mirror_difference_except_the_registered_unsafe_level(
) -> Result<(), toml::de::Error> {
    for (needle, replacement) in [
        ("unsafe_code = \"deny\"", "unsafe_code = \"allow\""),
        ("unsafe_code = \"deny\"", "unsafe_code = \"forbid\""),
        ("unused_must_use = \"deny\"\n", ""),
        (
            "unfulfilled_lint_expectations = \"deny\"",
            "unfulfilled_lint_expectations = \"warn\"",
        ),
        ("check-cfg = ['cfg(kani)']", "check-cfg = []"),
        ("priority = -1", "priority = 0"),
        ("all = { level = \"deny\"", "all = { level = \"warn\""),
        ("panic = \"deny\"", "panic = \"allow\""),
        ("unwrap_used = \"deny\"\n", ""),
        (
            "wildcard_enum_match_arm = \"deny\"",
            "wildcard_enum_match_arm = \"deny\"\nextra = \"deny\"",
        ),
    ] {
        assert_rejected(WORKSPACE, &MEMBER.replace(needle, replacement))?;
    }
    assert_rejected(
        &WORKSPACE.replace("unsafe_code = \"forbid\"", "unsafe_code = \"deny\""),
        MEMBER,
    )?;
    assert_rejected(
        &WORKSPACE.replace("panic = \"deny\"", "panic = \"deny\"\nextra = \"deny\""),
        MEMBER,
    )?;
    Ok(())
}
