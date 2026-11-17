use xtask::legacy_audit::{assess, AuditError};

const MANIFEST: &str = r#"{"private":true,"devDependencies":{"ari-client":"2.2.0"}}"#;
const LOCK: &str = include_str!("../../tests/interop/node-ari-client/package-lock.json");
const REPORT: &str = r#"{"auditReportVersion":2,"vulnerabilities":{
"ari-client":{"name":"ari-client","severity":"moderate","via":["request"],"nodes":["node_modules/ari-client"]},
"request":{"name":"request","severity":"critical","via":["form-data"],"nodes":["node_modules/request"]},
"form-data":{"name":"form-data","severity":"critical","via":[{"name":"form-data","dependency":"form-data","url":"https://github.com/advisories/GHSA-fjxv-7rqg-78g4"},{"name":"form-data","dependency":"form-data","url":"https://github.com/advisories/GHSA-hmw2-7cc7-3qxx"}],"nodes":["node_modules/form-data"]}},
"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":2,"total":3}}}"#;

fn registry_lock() -> Result<String, AuditError> {
    let mut lock: serde_json::Value = serde_json::from_str(LOCK)?;
    let packages = lock["packages"]
        .as_object_mut()
        .ok_or(AuditError::Boundary)?;
    packages.retain(|path, _| {
        !matches!(
            path.as_str(),
            "project-service-only" | "node_modules/tinyglobby"
        )
    });
    Ok(lock.to_string())
}

#[test]
fn only_exact_private_fixture_dependencies_and_scoped_advisories_pass() -> Result<(), AuditError> {
    let registry_lock = registry_lock()?;
    let lock_text = registry_lock.as_str();
    let assessment = assess(MANIFEST, lock_text, REPORT)?;
    assert_eq!(assessment.packages, 3);
    assert_eq!(assessment.advisories, 2);
    assert!(matches!(
        assess(&MANIFEST.replace("true", "false"), lock_text, REPORT),
        Err(AuditError::Boundary)
    ));
    assert!(matches!(
        assess(MANIFEST, &lock_text.replace("2.88.2", "2.88.3"), REPORT),
        Err(AuditError::ChangedVersion(_))
    ));
    assert!(matches!(
        assess(
            MANIFEST,
            lock_text,
            &REPORT.replace("GHSA-fjxv-7rqg-78g4", "GHSA-unknown")
        ),
        Err(AuditError::UnreviewedAdvisory(_))
    ));
    assert!(matches!(
        assess(
            MANIFEST,
            lock_text,
            &REPORT.replace("[\"request\"]", "[\"ari-client\"]")
        ),
        Err(AuditError::InvalidCause)
    ));
    assert!(matches!(
        assess(
            MANIFEST,
            lock_text,
            &REPORT.replace("\"total\":3", "\"total\":4")
        ),
        Err(AuditError::InvalidReport)
    ));
    let mut report: serde_json::Value = serde_json::from_str(REPORT)?;
    report["vulnerabilities"]["eslint"] = report["vulnerabilities"]["request"].clone();
    report["vulnerabilities"]["eslint"]["name"] = "eslint".into();
    report["metadata"]["vulnerabilities"]["total"] = 4.into();
    report["metadata"]["vulnerabilities"]["critical"] = 3.into();
    assert!(matches!(
        assess(MANIFEST, lock_text, &report.to_string()),
        Err(AuditError::UnexpectedPackage(_))
    ));
    assert!(assess(MANIFEST, lock_text, "{}").is_err());
    assert!(matches!(
        assess(MANIFEST, lock_text, &" ".repeat(1024 * 1024 + 1)),
        Err(AuditError::Capacity)
    ));
    let mut manifest: serde_json::Value = serde_json::from_str(MANIFEST)?;
    manifest["dependencies"] = serde_json::from_str(r#"{"ari-client":"2.2.0"}"#)?;
    assert!(matches!(
        assess(&manifest.to_string(), lock_text, REPORT),
        Err(AuditError::Boundary)
    ));
    let mut lock: serde_json::Value = serde_json::from_str(lock_text)?;
    lock["packages"]["node_modules/request"]["resolved"] = "file:replacement".into();
    assert!(matches!(
        assess(MANIFEST, &lock.to_string(), REPORT),
        Err(AuditError::Boundary)
    ));
    let mut lock: serde_json::Value = serde_json::from_str(lock_text)?;
    lock["packages"]["node_modules/eslint"]["license"] = "GPL-3.0-only".into();
    assert!(matches!(
        assess(MANIFEST, &lock.to_string(), REPORT),
        Err(AuditError::UnreviewedLicense(_))
    ));
    Ok(())
}
