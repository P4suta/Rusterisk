use xtask::distribution::SdkRuntimeIdentity;

fn lock(version: &str, source: &str, checksum: &str) -> Vec<u8> {
    format!("version=4\n[[package]]\nname='tokio'\nversion='{version}'\nsource='{source}'\nchecksum='{checksum}'\n").into_bytes()
}

#[test]
fn the_runtime_uses_the_exact_single_registry_identity_in_the_candidate_lock(
) -> Result<(), std::io::Error> {
    let checksum = "a".repeat(64);
    let bytes = lock("1.54.1", "registry+opaque-source", &checksum);
    let runtime = SdkRuntimeIdentity::from_lock(&bytes).map_err(std::io::Error::other)?;
    assert_eq!(runtime.version(), "1.54.1");
    assert_eq!(runtime.source(), "registry+opaque-source");
    assert_eq!(runtime.checksum(), checksum);
    Ok(())
}

#[test]
fn missing_duplicate_nonregistry_and_unchecked_runtime_identities_are_rejected() {
    let checksum = "a".repeat(64);
    for bytes in [
        b"version=4\n[[package]]\nname='other'\nversion='1.0.0'\n".to_vec(),
        b"version=4\n[[package]]\nname='tokio'\nversion='1.54.1'\n".to_vec(),
        lock("1.54.1", "git+opaque-source", &checksum),
        lock("1.54.1", "registry+opaque-source", "not-a-checksum"),
        lock("latest", "registry+opaque-source", &checksum),
        lock("1.54.1", "registry+", &checksum),
    ] {
        assert!(SdkRuntimeIdentity::from_lock(&bytes).is_err());
    }
    let mut duplicate = lock("1.54.1", "registry+opaque-source", &checksum);
    duplicate.extend_from_slice(
        format!("[[package]]\nname='tokio'\nversion='1.54.0'\nsource='registry+opaque-source'\nchecksum='{checksum}'\n").as_bytes(),
    );
    assert!(SdkRuntimeIdentity::from_lock(&duplicate).is_err());
}
