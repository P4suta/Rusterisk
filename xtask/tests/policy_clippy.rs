use std::{
    io,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn resolved_aliases_and_wrappers_cannot_bypass_shared_ownership_policy() -> Result<(), io::Error> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-policy-clippy-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let fixtures = [
            ("owned", "pub fn value() -> u8 { 1 }", true),
            (
                "qualified",
                "pub fn value() { let _shared = ::std::sync::Arc::new(1_u8); }",
                false,
            ),
            (
                "alias",
                "use std::sync::Arc as Shared; pub fn value() { let _shared = Shared::new(1_u8); }",
                false,
            ),
            (
                "type_alias",
                "type Shared<T> = std::sync::Arc<T>; pub fn value() { let _shared = Shared::new(1_u8); }",
                false,
            ),
            (
                "wrapper",
                "pub struct State(pub std::sync::Arc<u8>);",
                false,
            ),
            (
                "other_function",
                "pub fn other_connector() { let _shared = ::std::sync::Arc::new(1_u8); }",
                false,
            ),
            (
                "unused_expectation",
                "#[expect(clippy::disallowed_types, reason = \"unused fixture expectation\")] pub fn value() -> u8 { 1 }",
                false,
            ),
        ];
        for (name, source, allowed) in fixtures {
            let path = directory.join(format!("{name}.rs"));
            std::fs::write(&path, source)?;
            let output = Command::new("clippy-driver")
                .current_dir(&repository)
                .env("CLIPPY_CONF_DIR", &repository)
                .stdin(Stdio::null())
                .args([
                    "--edition=2024",
                    "--crate-type=lib",
                    "--emit=metadata",
                    "-Dwarnings",
                    "-Dclippy::disallowed_types",
                    "-Dunfulfilled_lint_expectations",
                    "--out-dir",
                ])
                .arg(&directory)
                .arg(&path)
                .output()?;
            if output.status.success() != allowed {
                return Err(io::Error::other(format!(
                    "{name}: {}\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            if !allowed
                && !String::from_utf8_lossy(&output.stderr).contains(
                    if name == "unused_expectation" {
                        "unfulfilled"
                    } else {
                        "disallowed"
                    },
                )
            {
                return Err(io::Error::other(format!(
                    "{name}: missing policy diagnostic: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(directory);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; cleanup failed: {cleanup}"
        ))),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
    }
}
