use xtask::distribution::{check_sdk_consumer_lock, SdkConsumerScope};

fn plain(name: &str, version: &str) -> String {
    format!("[[package]]\nname='{name}'\nversion='{version}'\n")
}

fn registry(name: &str, version: &str, checksum: char) -> String {
    format!(
        "{}source='registry+opaque-source'\nchecksum='{}'\n",
        plain(name, version),
        checksum.to_string().repeat(64)
    )
}

fn seed() -> String {
    format!(
        "version=4\n{}{}{}{}{}",
        plain("rusterisk-client", "0.1.0"),
        registry("rusterisk-core", "0.1.0", '0'),
        registry("rusterisk-api", "0.1.0", '1'),
        registry("serde", "1.0.229", '2'),
        registry("unused-registry-crate", "1.2.3", '3')
    )
}

fn resolved() -> String {
    format!(
        "version=4\n{}{}{}{}{}",
        plain("rusterisk-sdk-candidate-consumer", "0.0.0"),
        plain("rusterisk-core", "0.1.0"),
        plain("rusterisk-api", "0.1.0"),
        plain("rusterisk-client", "0.1.0"),
        registry("serde", "1.0.229", '2')
    )
}

#[test]
fn lock_updates_may_prune_unused_registry_packages_and_bind_sdk_paths() -> Result<(), std::io::Error>
{
    check_sdk_consumer_lock(
        SdkConsumerScope::Sdk,
        "0.1.0",
        seed().as_bytes(),
        resolved().as_bytes(),
    )
    .map_err(std::io::Error::other)?;
    let core = format!("version=4\n{}", plain("rusterisk-core", "0.1.0"));
    let consumer = format!(
        "{core}{}",
        plain("rusterisk-core-candidate-consumer", "0.0.0")
    );
    check_sdk_consumer_lock(
        SdkConsumerScope::CoreMsrv,
        "0.1.0",
        core.as_bytes(),
        consumer.as_bytes(),
    )
    .map_err(std::io::Error::other)
}

#[test]
fn registry_versions_sources_and_checksums_must_match_candidate_identities() {
    let original = resolved();
    for changed in [
        original.replace("version='1.0.229'", "version='1.0.230'"),
        original.replace("registry+opaque-source", "registry+another-source"),
        original.replace(&"2".repeat(64), &"3".repeat(64)),
        format!("{original}{}", registry("new-registry-crate", "1.0.0", '2')),
        original.replace(&format!("checksum='{}'\n", "2".repeat(64)), ""),
    ] {
        assert!(check_sdk_consumer_lock(
            SdkConsumerScope::Sdk,
            "0.1.0",
            seed().as_bytes(),
            changed.as_bytes()
        )
        .is_err());
    }
}

#[test]
fn all_selected_sdk_crates_and_the_exact_consumer_must_remain_local() {
    let original = resolved();
    for changed in [
        original.replace(&plain("rusterisk-api", "0.1.0"), ""),
        original.replace("name='rusterisk-api'", "name='rusterisk-server'"),
        original.replace(
            &plain("rusterisk-core", "0.1.0"),
            &registry("rusterisk-core", "0.1.0", '0'),
        ),
        original.replace("version='0.0.0'", "version='0.1.0'"),
        original.replace(
            &plain("rusterisk-client", "0.1.0"),
            &plain("rusterisk-client", "0.2.0"),
        ),
        format!("{original}{}", plain("checkout-helper", "1.0.0")),
        format!("{original}{}", plain("rusterisk-core", "0.1.0")),
    ] {
        assert!(check_sdk_consumer_lock(
            SdkConsumerScope::Sdk,
            "0.1.0",
            seed().as_bytes(),
            changed.as_bytes()
        )
        .is_err());
    }
}

#[test]
fn seed_lockfiles_are_validated_before_becoming_allowed_registry_identities() {
    let original = seed();
    for changed in [
        original.replace("version=4", "version=3"),
        original.replace(&plain("rusterisk-client", "0.1.0"), ""),
        original.replace("name='rusterisk-api'", "name='rusterisk-media'"),
        format!("{original}{}", registry("serde", "1.0.229", '2')),
        original.replace("registry+opaque-source", "git+opaque-source"),
        original.replace(&"2".repeat(64), &"A".repeat(64)),
    ] {
        assert!(check_sdk_consumer_lock(
            SdkConsumerScope::Sdk,
            "0.1.0",
            changed.as_bytes(),
            resolved().as_bytes()
        )
        .is_err());
    }
}

#[test]
fn the_core_consumer_cannot_acquire_any_registry_dependency() {
    let core = format!("version=4\n{}", plain("rusterisk-core", "0.1.0"));
    let consumer = format!(
        "{core}{}{}",
        plain("rusterisk-core-candidate-consumer", "0.0.0"),
        registry("serde", "1.0.229", '2')
    );
    assert!(check_sdk_consumer_lock(
        SdkConsumerScope::CoreMsrv,
        "0.1.0",
        core.as_bytes(),
        consumer.as_bytes()
    )
    .is_err());
}

#[test]
fn lock_package_counts_accept_the_boundary_and_reject_an_additional_record(
) -> Result<(), std::io::Error> {
    let mut candidate = seed();
    let mut consumer = resolved();
    for index in 0..1019 {
        let entry = registry(&format!("registry-crate-{index}"), "1.0.0", '4');
        candidate.push_str(&entry);
        consumer.push_str(&entry);
    }
    check_sdk_consumer_lock(
        SdkConsumerScope::Sdk,
        "0.1.0",
        candidate.as_bytes(),
        consumer.as_bytes(),
    )
    .map_err(std::io::Error::other)?;
    let additional = registry("one-more-registry-crate", "1.0.0", '4');
    consumer.push_str(&additional);
    assert!(matches!(
        check_sdk_consumer_lock(
            SdkConsumerScope::Sdk,
            "0.1.0",
            candidate.as_bytes(),
            consumer.as_bytes()
        ),
        Err(xtask::distribution::SdkConsumerLockError::Size)
    ));
    candidate.push_str(&additional);
    assert!(matches!(
        check_sdk_consumer_lock(
            SdkConsumerScope::Sdk,
            "0.1.0",
            candidate.as_bytes(),
            consumer.as_bytes()
        ),
        Err(xtask::distribution::SdkConsumerLockError::Size)
    ));
    assert!(check_sdk_consumer_lock(
        SdkConsumerScope::Sdk,
        "0.1.0",
        &[0xff, 0xfe],
        resolved().as_bytes()
    )
    .is_err());
    Ok(())
}

#[test]
fn lockfiles_have_bounded_text_and_reject_duplicate_or_unknown_fields() -> Result<(), std::io::Error>
{
    let mut padded = resolved().into_bytes();
    padded.resize(2 * 1024 * 1024, b' ');
    check_sdk_consumer_lock(SdkConsumerScope::Sdk, "0.1.0", seed().as_bytes(), &padded)
        .map_err(std::io::Error::other)?;
    padded.push(b' ');
    assert!(
        check_sdk_consumer_lock(SdkConsumerScope::Sdk, "0.1.0", seed().as_bytes(), &padded)
            .is_err()
    );
    for changed in [
        "[broken".to_owned(),
        "".to_owned(),
        resolved().replace("version=4", "version=4\nversion=4"),
        resolved().replace("version=4", "version=4\nredirect='checkout'"),
    ] {
        assert!(check_sdk_consumer_lock(
            SdkConsumerScope::Sdk,
            "0.1.0",
            seed().as_bytes(),
            changed.as_bytes()
        )
        .is_err());
    }
    Ok(())
}
