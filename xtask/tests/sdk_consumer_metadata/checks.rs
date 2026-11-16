use super::fixture::{checkout, metadata, plan};
use xtask::distribution::{check_sdk_consumer_metadata, SdkConsumerScope};

#[test]
fn metadata_binds_opaque_package_ids_to_the_extracted_candidates_and_consumer_root(
) -> Result<(), std::io::Error> {
    let plan = plan()?;
    for scope in SdkConsumerScope::ALL {
        let data = metadata(&plan, scope);
        check_sdk_consumer_metadata(&plan, scope, "0.1.0", &data.bytes()?)
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

#[test]
fn format_one_allows_additional_fields_and_refuses_duplicate_required_fields(
) -> Result<(), std::io::Error> {
    let plan = plan()?;
    let original = metadata(&plan, SdkConsumerScope::Sdk).bytes()?;
    let rest = original
        .get(1..)
        .ok_or_else(|| std::io::Error::other("fixture JSON missing"))?;
    let mut extended = br#"{"future_metadata":{"value":"ignored"},"#.to_vec();
    extended.extend_from_slice(rest);
    check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &extended)
        .map_err(std::io::Error::other)?;
    let mut duplicate = br#"{"version":1,"#.to_vec();
    duplicate.extend_from_slice(rest);
    assert!(
        check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &duplicate).is_err()
    );
    Ok(())
}

#[test]
fn sdk_packages_cannot_resolve_to_checkout_registry_or_another_version(
) -> Result<(), std::io::Error> {
    let plan = plan()?;
    for variant in 0..4 {
        let mut data = metadata(&plan, SdkConsumerScope::Sdk);
        let core = data.package("rusterisk-core")?;
        match variant {
            0 => core.manifest_path = checkout(&plan).join("crates/core/Cargo.toml"),
            1 => core.source = Some("registry+opaque-source".to_owned()),
            2 => core.version = "0.2.0".to_owned(),
            _ => core.targets[0].src_path = checkout(&plan).join("core.rs"),
        }
        assert!(
            check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &data.bytes()?)
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn backend_packages_path_dependencies_and_unselected_local_crates_are_rejected(
) -> Result<(), std::io::Error> {
    let plan = plan()?;
    for variant in 0..4 {
        let mut data = metadata(&plan, SdkConsumerScope::Sdk);
        data.add_external(&plan, 0)?;
        let extra = data.package("registry-package-0")?;
        match variant {
            0 => extra.name = "rusterisk-server".to_owned(),
            1 => extra.source = None,
            2 => extra.source = Some("git+opaque-source".to_owned()),
            _ => {
                data.package("rusterisk-client")?.dependencies[0].path =
                    Some(checkout(&plan).join("crates/core"));
            }
        }
        assert!(
            check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &data.bytes()?)
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn missing_graph_links_wrong_workspace_and_duplicate_sdk_packages_are_rejected(
) -> Result<(), std::io::Error> {
    let plan = plan()?;
    for variant in 0..7 {
        let mut data = metadata(&plan, SdkConsumerScope::Sdk);
        match variant {
            0 => data.resolve = None,
            1 => data.workspace_root = checkout(&plan),
            2 => data.target_directory = checkout(&plan).join("target"),
            3 => data.workspace_members[0] = "another-consumer".to_owned(),
            4 => data.node("rusterisk-client")?.dependencies.clear(),
            5 => {
                data.node("rusterisk-sdk-candidate-consumer")?.deps[0].dep_kinds[0].kind =
                    Some("dev".to_owned())
            }
            _ => {
                let copy = data.package("rusterisk-core")?.clone();
                data.packages.push(copy);
            }
        }
        assert!(
            check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &data.bytes()?)
                .is_err(),
            "{variant}"
        );
    }
    Ok(())
}

#[test]
fn the_core_msrv_consumer_remains_dependency_free_beyond_the_core_package(
) -> Result<(), std::io::Error> {
    let plan = plan()?;
    let mut data = metadata(&plan, SdkConsumerScope::CoreMsrv);
    data.add_external(&plan, 0)?;
    assert!(check_sdk_consumer_metadata(
        &plan,
        SdkConsumerScope::CoreMsrv,
        "0.1.0",
        &data.bytes()?
    )
    .is_err());
    Ok(())
}

#[test]
fn json_and_graph_sizes_have_exact_bounds() -> Result<(), std::io::Error> {
    let plan = plan()?;
    let mut data = metadata(&plan, SdkConsumerScope::Sdk);
    let mut padded = data.bytes()?;
    padded.resize(8 * 1024 * 1024, b' ');
    check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &padded)
        .map_err(std::io::Error::other)?;
    padded.push(b' ');
    assert!(check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &padded).is_err());
    for index in 0..1020 {
        data.add_external(&plan, index)?;
    }
    check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &data.bytes()?)
        .map_err(std::io::Error::other)?;
    data.add_external(&plan, 1020)?;
    assert!(
        check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &data.bytes()?).is_err()
    );
    for bytes in [b"{".as_slice(), &[0xff, 0xfe], b""] {
        assert!(check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", bytes).is_err());
    }
    Ok(())
}
