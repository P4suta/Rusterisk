use std::{collections::BTreeMap, process::Command};

fn fixture() -> BTreeMap<String, String> {
    let mut files = BTreeMap::from([
        ("Cargo.toml".to_owned(), "[workspace]\nmembers = ['crates/core', 'crates/api', 'crates/client']\n[workspace.package]\nversion = '0.1.0'\nlicense = 'MIT OR Apache-2.0'\nrepository = 'https://github.com/P4suta/Rusterisk'\n[workspace.dependencies]\nforbidden = { path = 'private' }\n".to_owned()),
        ("docs/rust-sdk.md".to_owned(), "# Rust libraries\n".to_owned()),
        ("LICENSE-MIT".to_owned(), "MIT fixture\n".to_owned()),
        ("LICENSE-APACHE".to_owned(), "Apache fixture\n".to_owned()),
    ]);
    for name in ["core", "api", "client"] {
        files.insert(format!("crates/{name}/Cargo.toml"), format!("[package]\nname = 'rusterisk-{name}'\nlicense.workspace = true\nrepository.workspace = true\ndescription = 'Typed Rust library'\nreadme = '../../docs/rust-sdk.md'\n"));
        for (file, text) in [
            ("LICENSE-MIT", "MIT fixture\n"),
            ("LICENSE-APACHE", "Apache fixture\n"),
        ] {
            files.insert(format!("crates/{name}/{file}"), text.to_owned());
        }
    }
    files.insert("crates/client/Cargo.toml".to_owned(), "[package]\nname = 'rusterisk-client'\nlicense.workspace = true\nrepository.workspace = true\ndescription = 'Typed Rust library'\nreadme = '../../docs/rust-sdk.md'\nexclude = ['tests/**', 'examples/**']\n[dependencies]\nrusterisk-api = { version = '0.1.0', path = '../api' }\nrusterisk-core = { version = '0.1.0', path = '../core' }\n[dev-dependencies]\nrusterisk-server = { path = '../server' }\nrusterisk-config = { path = '../config' }\nrusterisk-media = { path = '../media' }\n".to_owned());
    files
}

#[test]
fn fast_and_security_gates_reject_unapproved_paths_before_external_effects(
) -> Result<(), std::io::Error> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-dependency-path-cli-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let operation = (|| {
        for (relative, contents) in fixture() {
            let path = root.join(relative);
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("fixture parent missing"))?;
            std::fs::create_dir_all(parent)?;
            std::fs::write(path, contents)?;
        }
        let run = |task| {
            Command::new(env!("CARGO_BIN_EXE_xtask"))
                .arg(task)
                .current_dir(&root)
                .env("PATH", "")
                .output()
        };
        let outputs = [run("distribution-check")?, run("check")?, run("security")?];
        Ok::<_, std::io::Error>((outputs, root.join(".cache").exists()))
    })();
    let cleanup = std::fs::remove_dir_all(root);
    let (outputs, cache_created) = match (operation, cleanup) {
        (Ok(result), Ok(())) => result,
        (Err(operation), Err(cleanup)) => {
            return Err(std::io::Error::other(format!(
                "{operation}; fixture cleanup failed: {cleanup}"
            )))
        }
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
    };
    for output in outputs {
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("Cargo.toml: forbidden requires a version"),
            "{error}"
        );
        assert!(
            output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert!(!cache_created);
    Ok(())
}
