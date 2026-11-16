#[path = "sdk_http_metadata/fixture.rs"]
mod http_fixture;
#[path = "sdk_consumer_metadata/fixture.rs"]
mod metadata_fixture;

use http_fixture::{bytes, data, runtime};
use metadata_fixture::{self as fixture, Dependency, Edge, Kind};
use xtask::distribution::{check_sdk_consumer_metadata, check_sdk_http_metadata, SdkConsumerScope};

#[test]
fn http_consumers_bind_the_only_added_direct_dependency_to_the_candidate_runtime(
) -> Result<(), std::io::Error> {
    let plan = fixture::plan()?;
    let data = data(&plan)?;
    let bytes = bytes(&data, 0)?;
    check_sdk_http_metadata(&plan, "0.1.0", &runtime()?, &bytes).map_err(std::io::Error::other)?;
    assert!(check_sdk_consumer_metadata(&plan, SdkConsumerScope::Sdk, "0.1.0", &bytes).is_err());
    let missing = fixture::metadata(&plan, SdkConsumerScope::Sdk).bytes()?;
    assert!(check_sdk_http_metadata(&plan, "0.1.0", &runtime()?, &missing).is_err());
    Ok(())
}

#[test]
fn changed_runtime_versions_sources_features_and_extra_direct_dependencies_are_rejected(
) -> Result<(), std::io::Error> {
    let plan = fixture::plan()?;
    for variant in 0..10 {
        let mut data = data(&plan)?;
        match variant {
            0 => data.package("tokio")?.version = "1.54.0".to_owned(),
            1 => data.package("tokio")?.source = Some("registry+different-source".to_owned()),
            2 => {
                data.package(SdkConsumerScope::Sdk.package_name())?
                    .dependencies[3]
                    .req = "^1.54.1".to_owned()
            }
            3 | 4 => {}
            5 => {
                data.package(SdkConsumerScope::Sdk.package_name())?
                    .dependencies[3]
                    .kind = Some("dev".to_owned())
            }
            6 => {
                data.package(SdkConsumerScope::Sdk.package_name())?
                    .dependencies[3]
                    .target = Some("cfg(unix)".to_owned())
            }
            7 => {
                let extra = data
                    .package(SdkConsumerScope::Sdk.package_name())?
                    .dependencies[0]
                    .clone();
                data.package(SdkConsumerScope::Sdk.package_name())?
                    .dependencies
                    .push(extra);
            }
            8 => {
                data.add_external(&plan, 1)?;
                let extra = data.package("registry-package-1")?.clone();
                data.package(SdkConsumerScope::Sdk.package_name())?
                    .dependencies
                    .push(Dependency {
                        name: extra.name.clone(),
                        req: "=1.0.0".to_owned(),
                        source: extra.source,
                        path: None,
                        kind: None,
                        target: None,
                    });
                let root = data.node(SdkConsumerScope::Sdk.package_name())?;
                root.dependencies.push(extra.id.clone());
                root.deps.push(Edge {
                    name: extra.name.replace('-', "_"),
                    pkg: extra.id,
                    dep_kinds: [Kind {
                        kind: None,
                        target: None,
                    }],
                });
            }
            _ => data.package("tokio")?.manifest_path = fixture::checkout(&plan).join("Cargo.toml"),
        }
        assert!(
            check_sdk_http_metadata(&plan, "0.1.0", &runtime()?, &bytes(&data, variant)?).is_err(),
            "variant {variant}"
        );
    }
    Ok(())
}
