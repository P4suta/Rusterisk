use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xtask::{
    browser::{check_with, BrowserError, Declaration},
    process::ProcessError,
};

fn fixture() -> Result<PathBuf, BrowserError> {
    let identity = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BrowserError::Package)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-browser-check-{}-{identity}",
        std::process::id()
    ));
    let package = root.join("examples/browser/node_modules/sip.js");
    std::fs::create_dir_all(&package)?;
    std::fs::write(
        package.join("package.json"),
        "{\"name\":\"sip.js\",\"version\":\"0.21.2\"}",
    )?;
    let actual =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/browser/node_modules/sip.js");
    for declaration in Declaration::ALL {
        let path = package.join(declaration.path());
        std::fs::create_dir_all(path.parent().ok_or(BrowserError::Package)?)?;
        std::fs::copy(actual.join(declaration.path()), path)?;
    }
    Ok(root)
}

#[test]
fn browser_bootstrap_stops_before_tests_and_build_when_strict_check_fails(
) -> Result<(), BrowserError> {
    let root = fixture()?;
    let mut phases = Vec::new();
    let result = check_with(&root, |command: &mut Command, budget: Duration| {
        assert!(budget.as_secs() <= 120);
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        let strict = args == ["run", "check"];
        phases.push(args);
        if strict {
            Err(ProcessError::InvalidOutput)
        } else {
            Ok(())
        }
    });
    std::fs::remove_dir_all(root)?;
    assert!(result.is_err());
    assert_eq!(phases.len(), 2);
    assert_eq!(
        phases[0],
        ["ci", "--ignore-scripts", "--no-audit", "--no-fund"]
    );
    Ok(())
}

#[test]
fn browser_bootstrap_completes_type_preparation_and_every_owned_script() -> Result<(), BrowserError>
{
    let root = fixture()?;
    let mut phases = Vec::new();
    let result = check_with(&root, |command: &mut Command, budget: Duration| {
        assert!(budget.as_secs() <= 120);
        phases.push(
            command
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        );
        Ok(())
    });
    let corrected = std::fs::read_to_string(
        root.join("examples/browser/node_modules/sip.js")
            .join(Declaration::Incoming.path()),
    )?;
    std::fs::remove_dir_all(root)?;
    result?;
    assert!(corrected.contains(Declaration::Incoming.patched()));
    assert_eq!(
        phases,
        [
            ["ci", "--ignore-scripts", "--no-audit", "--no-fund"]
                .map(str::to_owned)
                .to_vec(),
            ["run", "check"].map(str::to_owned).to_vec(),
            ["test"].map(str::to_owned).to_vec(),
            ["run", "build"].map(str::to_owned).to_vec()
        ]
    );
    Ok(())
}
