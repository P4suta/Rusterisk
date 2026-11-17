use super::{fixture, read_json, write_json};
use serde_json::{json, Value};
use std::{fs, io, path::Path};
use xtask::legacy_audit::check_browser_fixture;

const ADAPTER: &str = "tests/interop/node-ari-client/project-service-only";
const LOCK_ENTRY: &str = "../../tests/interop/node-ari-client/project-service-only";
const MANIFEST: &str = include_str!("../../../examples/browser/package.json");
const LOCK: &str = include_str!("../../../examples/browser/package-lock.json");
const ESLINT: &str = include_str!("../../../examples/browser/eslint.config.mjs");
const TYPESCRIPT: &str = include_str!("../../../examples/browser/tsconfig.json");

fn using(check: impl FnOnce(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    fixture(|root| {
        let adapter = root.join(ADAPTER);
        fs::create_dir_all(
            adapter
                .parent()
                .ok_or_else(|| io::Error::other("Canonical adapter parent is missing"))?,
        )?;
        fs::rename(root.join("project-service-only"), &adapter)?;
        let browser = root.join("examples/browser");
        fs::create_dir_all(&browser)?;
        fs::write(browser.join("package.json"), MANIFEST)?;
        fs::write(browser.join("eslint.config.mjs"), ESLINT)?;
        fs::write(browser.join("tsconfig.json"), TYPESCRIPT)?;
        let mut lock: Value = serde_json::from_str(LOCK).map_err(io::Error::other)?;
        let packages = lock["packages"]
            .as_object_mut()
            .ok_or_else(|| io::Error::other("Browser lock package inventory is missing"))?;
        packages.retain(|path, _| {
            !matches!(
                path.as_str(),
                "node_modules/tinyglobby" | "node_modules/fdir" | "node_modules/picomatch"
            )
        });
        packages.insert(
            "node_modules/tinyglobby".into(),
            json!({ "resolved": LOCK_ENTRY, "link": true }),
        );
        packages.insert(
            LOCK_ENTRY.into(),
            json!({
                "name": "@rusterisk/project-service-only",
                "dev": true,
                "license": "(MIT OR Apache-2.0)"
            }),
        );
        write_json(&browser.join("package-lock.json"), &lock)?;
        check(root, &browser)
    })
}

#[test]
fn browser_profile_accepts_the_single_canonical_shared_adapter() -> io::Result<()> {
    using(|_, browser| check_browser_fixture(browser).map_err(io::Error::other))
}

#[test]
fn browser_profile_rejects_additional_local_dependencies_omitted_by_npm_audit() -> io::Result<()> {
    using(|root, browser| {
        let unreviewed = root.join("unreviewed");
        fs::create_dir(&unreviewed)?;
        write_json(
            &unreviewed.join("package.json"),
            &json!({ "name": "unreviewed", "private": true, "license": "MIT" }),
        )?;
        fs::write(unreviewed.join("index.mjs"), b"export {};\n")?;
        let path = browser.join("package.json");
        let mut manifest = read_json(&path)?;
        manifest["devDependencies"]["unreviewed"] = "file:../../unreviewed".into();
        write_json(&path, &manifest)?;
        let path = browser.join("package-lock.json");
        let mut lock = read_json(&path)?;
        lock["packages"]["node_modules/unreviewed"] =
            json!({ "resolved": "../../unreviewed", "link": true });
        lock["packages"]["../../unreviewed"] =
            json!({ "name": "unreviewed", "dev": true, "license": "MIT" });
        write_json(&path, &lock)?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })
}

#[test]
fn browser_profile_rejects_noncanonical_overrides_and_link_destinations() -> io::Result<()> {
    using(|_, browser| {
        let path = browser.join("package.json");
        let mut manifest = read_json(&path)?;
        manifest["overrides"]["@typescript-eslint/typescript-estree"]["tinyglobby"] =
            "file:./project-service-only".into();
        write_json(&path, &manifest)?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })?;
    using(|_, browser| {
        let path = browser.join("package-lock.json");
        let mut lock = read_json(&path)?;
        lock["packages"]["node_modules/tinyglobby"]["resolved"] = "../../other".into();
        write_json(&path, &lock)?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })
}

#[test]
fn browser_profile_rejects_nonregistry_sources_and_changed_runtime_pins() -> io::Result<()> {
    for source in [
        "file:../../unreviewed",
        "git+https://example.invalid/unreviewed.git",
    ] {
        using(|_, browser| {
            let path = browser.join("package.json");
            let mut manifest = read_json(&path)?;
            manifest["devDependencies"]["eslint"] = source.into();
            write_json(&path, &manifest)?;
            assert!(check_browser_fixture(browser).is_err());
            Ok(())
        })?;
        using(|_, browser| {
            let path = browser.join("package-lock.json");
            let mut lock = read_json(&path)?;
            lock["packages"]["node_modules/eslint"]["resolved"] = source.into();
            write_json(&path, &lock)?;
            assert!(check_browser_fixture(browser).is_err());
            Ok(())
        })?;
    }
    using(|_, browser| {
        let path = browser.join("package-lock.json");
        let mut lock = read_json(&path)?;
        lock["packages"]["node_modules/sip.js"]["version"] = "0.21.1".into();
        write_json(&path, &lock)?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })
}

#[test]
fn browser_profile_rejects_changed_shared_sources_and_project_service_policy() -> io::Result<()> {
    using(|root, browser| {
        fs::write(root.join(ADAPTER).join("index.ts"), b"unreviewed source\n")?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })?;
    using(|root, browser| {
        fs::remove_file(root.join(ADAPTER).join("index.mjs"))?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })?;
    using(|_, browser| {
        fs::write(
            browser.join("eslint.config.mjs"),
            ESLINT.replace("projectService: true", "projectService: false"),
        )?;
        assert!(check_browser_fixture(browser).is_err());
        Ok(())
    })
}
