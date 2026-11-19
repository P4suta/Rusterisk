use serde_json::{json, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use xtask::legacy_audit::{assess, assess_fixture, AuditError};

#[path = "project_service_profile/browser.rs"]
mod browser;

const MANIFEST: &str = include_str!("../../tests/interop/node-ari-client/package.json");
const LOCK: &str = include_str!("../../tests/interop/node-ari-client/package-lock.json");
const ESLINT: &str = include_str!("../../tests/interop/node-ari-client/eslint.config.mjs");
const TYPESCRIPT: &str = include_str!("../../tests/interop/node-ari-client/tsconfig.json");
const REPORT: &str = r#"{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}"#;
const ENTRY: &str = "export { globSync, isDynamicPattern } from \"./index.ts\";\n";
const SOURCE: &str = "export function isDynamicPattern(): never {\n\tthrow new Error(\n\t\t\"Explicit project lists are unsupported; use projectService: true.\",\n\t);\n}\n\nexport function globSync(): never {\n\tthrow new Error(\n\t\t\"Explicit project lists are unsupported; use projectService: true.\",\n\t);\n}\n";
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn read_json(path: &Path) -> io::Result<Value> {
    serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
}

fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(path, bytes)
}

fn write_fixture(root: &Path) -> io::Result<()> {
    let mut manifest: Value = serde_json::from_str(MANIFEST).map_err(io::Error::other)?;
    manifest["overrides"]["@typescript-eslint/typescript-estree"]["tinyglobby"] =
        "file:../../../project-service-only".into();
    write_json(&root.join("package.json"), &manifest)?;
    fs::write(root.join("eslint.config.mjs"), ESLINT)?;
    fs::write(root.join("tsconfig.json"), TYPESCRIPT)?;
    let adapter = json!({
        "name": "@rusterisk/project-service-only",
        "private": true,
        "type": "module",
        "license": "(MIT OR Apache-2.0)",
        "exports": "./index.mjs"
    });
    fs::create_dir(root.join("project-service-only"))?;
    write_json(&root.join("project-service-only/package.json"), &adapter)?;
    fs::write(root.join("project-service-only/index.mjs"), ENTRY)?;
    fs::write(root.join("project-service-only/index.ts"), SOURCE)?;
    let mut lock: Value = serde_json::from_str(LOCK).map_err(io::Error::other)?;
    let packages = lock["packages"]
        .as_object_mut()
        .ok_or_else(|| io::Error::other("Fixture lock has no package inventory"))?;
    packages.retain(|path, _| {
        !matches!(
            path.as_str(),
            "node_modules/tinyglobby" | "node_modules/fdir" | "node_modules/picomatch"
        )
    });
    packages.insert(
        "node_modules/tinyglobby".into(),
        json!({ "resolved": "project-service-only", "link": true }),
    );
    packages.insert(
        "project-service-only".into(),
        json!({
            "name": "@rusterisk/project-service-only",
            "dev": true,
            "license": "(MIT OR Apache-2.0)"
        }),
    );
    write_json(&root.join("package-lock.json"), &lock)
}

fn fixture_directory(stamp: u128) -> PathBuf {
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rusterisk-project-service-profile-{}-{stamp}-{sequence}",
        std::process::id()
    ))
}

fn fixture(check: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let directory = fixture_directory(stamp);
    fs::create_dir(&directory)?;
    let result = write_fixture(&directory).and_then(|()| check(&directory));
    let cleanup = fs::remove_dir_all(directory);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn fixture_paths_differ_within_one_clock_tick() {
    assert_ne!(fixture_directory(1), fixture_directory(1));
}

#[test]
fn exact_owned_profile_passes_without_opening_the_string_only_boundary() -> io::Result<()> {
    fixture(|root| {
        let assessment = assess_fixture(root, REPORT).map_err(io::Error::other)?;
        assert_eq!(assessment.packages, 0);
        assert_eq!(assessment.advisories, 0);
        let manifest = fs::read_to_string(root.join("package.json"))?;
        let lock = fs::read_to_string(root.join("package-lock.json"))?;
        assert!(matches!(
            assess(&manifest, &lock, REPORT),
            Err(AuditError::Boundary)
        ));
        Ok(())
    })
}

#[test]
fn project_service_and_explicit_project_changes_are_rejected() -> io::Result<()> {
    for changed in [
        ESLINT.replace("projectService: true", "projectService: false"),
        ESLINT.replace("projectService: true", "projectService: true, project: []"),
    ] {
        fixture(|root| {
            fs::write(root.join("eslint.config.mjs"), &changed)?;
            assert!(assess_fixture(root, REPORT).is_err());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn adapter_manifest_and_source_changes_are_rejected() -> io::Result<()> {
    for field in ["name", "exports", "dependencies", "scripts", "license"] {
        fixture(|root| {
            let path = root.join("project-service-only/package.json");
            let mut manifest = read_json(&path)?;
            manifest[field] = match field {
                "dependencies" => json!({ "picomatch": "4.0.7" }),
                "scripts" => json!({ "postinstall": "node index.mjs" }),
                "name" | "exports" | "license" => "unreviewed".into(),
                _ => return Err(io::Error::other("Unknown fixture manifest field")),
            };
            write_json(&path, &manifest)?;
            assert!(assess_fixture(root, REPORT).is_err());
            Ok(())
        })?;
    }
    fixture(|root| {
        fs::write(
            root.join("project-service-only/index.ts"),
            SOURCE.replace("throw new Error", "return new Error"),
        )?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })
}

#[test]
fn redirected_override_link_and_unrelated_local_dependency_are_rejected() -> io::Result<()> {
    fixture(|root| {
        let path = root.join("package.json");
        let mut manifest = read_json(&path)?;
        manifest["overrides"]["@typescript-eslint/typescript-estree"]["tinyglobby"] =
            "file:../project-service-only".into();
        write_json(&path, &manifest)?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })?;
    fixture(|root| {
        let path = root.join("package-lock.json");
        let mut lock = read_json(&path)?;
        lock["packages"]["node_modules/tinyglobby"]["resolved"] = "../outside".into();
        write_json(&path, &lock)?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })?;
    fixture(|root| {
        let path = root.join("package-lock.json");
        let mut lock = read_json(&path)?;
        lock["packages"]["node_modules/eslint"]["resolved"] = "file:replacement".into();
        write_json(&path, &lock)?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })
}

#[test]
fn unexpected_inventory_and_oversized_source_are_rejected() -> io::Result<()> {
    fixture(|root| {
        fs::write(root.join("project-service-only/extra.mjs"), "export {};\n")?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })?;
    fixture(|root| {
        fs::write(
            root.join("project-service-only/index.ts"),
            " ".repeat(1024 * 1024 + 1),
        )?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })
}

#[test]
fn an_unreviewed_advisory_cannot_be_hidden_by_local_provenance() -> io::Result<()> {
    fixture(|root| {
        let report = r#"{"auditReportVersion":2,"vulnerabilities":{"picomatch":{"name":"picomatch","severity":"high","via":[{"name":"picomatch","dependency":"picomatch","url":"https://github.com/advisories/GHSA-fc5q-39xc-vjpf"}],"nodes":["node_modules/picomatch"]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":1,"critical":0,"total":1}}}"#;
        assert!(assess_fixture(root, report).is_err());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn a_symlinked_adapter_source_is_rejected() -> io::Result<()> {
    fixture(|root| {
        fs::rename(
            root.join("project-service-only/index.ts"),
            root.join("original.ts"),
        )?;
        std::os::unix::fs::symlink("../original.ts", root.join("project-service-only/index.ts"))?;
        assert!(assess_fixture(root, REPORT).is_err());
        Ok(())
    })
}
