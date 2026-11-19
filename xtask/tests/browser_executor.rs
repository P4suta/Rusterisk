use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use xtask::{
    browser::{execute_with, Execution, RunError},
    process::ProcessError,
};

#[path = "fixtures/directory.rs"]
mod directory;

fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<(), std::io::Error> {
    let path = root.join(path);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("missing fixture parent"))?,
    )?;
    std::fs::write(path, bytes)
}

#[test]
fn browser_executor_stops_after_a_deadline_cleans_only_the_owned_container_and_removes_temporary_state(
) -> Result<(), std::io::Error> {
    let root = directory::reserve("browser-executor")?;
    let result = (|| {
        write(&root, "Cargo.toml", b"[workspace]\nmembers=['xtask']\n")?;
        for path in [
            "Cargo.lock",
            "xtask/Cargo.toml",
            "xtask/src/lib.rs",
            "xtask/tests/contract.rs",
            "xtask/fuzz/Cargo.toml",
            "xtask/fuzz/Cargo.lock",
            "xtask/fuzz/src/targets.rs",
            "tests/interop/node-ari-client/eslint.config.mjs",
            "tests/interop/node-ari-client/tsconfig.json",
        ] {
            write(&root, path, b"fixture")?;
        }
        for path in [
            "package.json",
            "package-lock.json",
            "tsconfig.json",
            "eslint.config.mjs",
            "biome.json",
            "playwright.config.ts",
            "index.html",
            "style.css",
            "src/main.ts",
            "tests/call.spec.ts",
            "config/http.conf",
            "config/ari.conf",
            "config/pjsip.conf",
            "config/rtp.conf",
            "config/extensions.conf",
        ] {
            write(&root, &format!("examples/browser/{path}"), b"fixture")?;
        }
        for file in ["package.json", "index.mjs", "index.ts"] {
            write(
                &root,
                &format!("tests/interop/node-ari-client/project-service-only/{file}"),
                b"fixture",
            )?;
        }
        let mut invocations = Vec::new();
        let mut temporary: Option<PathBuf> = None;
        let outcome = execute_with(
            &root,
            Execution::Run,
            &format!("sha256:{}", "a".repeat(64)),
            |command: &mut Command, budget: Duration| {
                let arguments: Vec<_> = command
                    .get_args()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect();
                assert!(budget.as_secs() <= 30);
                if arguments.first().is_some_and(|argument| argument == "run") {
                    for argument in &arguments {
                        if let Some(path) = argument
                            .strip_prefix("type=bind,source=")
                            .and_then(|argument| argument.strip_suffix(",target=/state"))
                        {
                            let state = PathBuf::from(path);
                            assert_eq!(
                                std::fs::read(state.join("browser/src/main.ts"))
                                    .map_err(ProcessError::Io)?,
                                b"fixture"
                            );
                            temporary = state.parent().map(Path::to_owned);
                            write(&root, "examples/browser/src/main.ts", b"concurrent edit")
                                .map_err(ProcessError::Io)?;
                        }
                    }
                    invocations.push(arguments);
                    Err(ProcessError::Timeout)
                } else {
                    assert_eq!(&arguments[..2], ["rm", "--force"]);
                    assert!(arguments[2].starts_with("rusterisk-browser-"));
                    assert!(arguments[2].ends_with("-rust-version"));
                    invocations.push(arguments);
                    Ok(())
                }
            },
        );
        assert!(matches!(
            outcome,
            Err(RunError::Process(ProcessError::Timeout))
        ));
        assert_eq!(invocations.len(), 2);
        let temporary = temporary.ok_or_else(|| std::io::Error::other("missing owned state"))?;
        assert!(!temporary.exists());
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(root);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{primary}; fixture cleanup failed: {cleanup}"
        ))),
    }
}
