#[path = "sdk_consumer_metadata/fixture.rs"]
mod fixture;

use fixture::{Dependency, Edge, Kind, Node};
use xtask::distribution::{
    check_sdk_consumer_metadata, check_sdk_http_metadata, SdkConsumerScope, SdkRuntimeIdentity,
};

fn runtime() -> Result<SdkRuntimeIdentity, std::io::Error> {
    let lock = format!("version=4\n[[package]]\nname='tokio'\nversion='1.54.1'\nsource='sparse+opaque-source'\nchecksum='{}'\n", "a".repeat(64));
    SdkRuntimeIdentity::from_lock(lock.as_bytes()).map_err(std::io::Error::other)
}

fn data(plan: &xtask::distribution::SdkConsumerPlan) -> Result<fixture::Metadata, std::io::Error> {
    let mut data = fixture::metadata(plan, SdkConsumerScope::Sdk);
    let mut tokio = fixture::external(plan, 0);
    tokio.name = "tokio".to_owned();
    tokio.id = "opaque-package:tokio".to_owned();
    tokio.version = "1.54.1".to_owned();
    let identifier = tokio.id.clone();
    data.packages.push(tokio);
    data.package(SdkConsumerScope::Sdk.package_name())?
        .dependencies
        .push(Dependency {
            name: "tokio".to_owned(),
            req: "=1.54.1".to_owned(),
            source: Some("sparse+opaque-source".to_owned()),
            path: None,
            kind: None,
            target: None,
        });
    let root = data.node(SdkConsumerScope::Sdk.package_name())?;
    root.dependencies.push(identifier.clone());
    root.deps.push(Edge {
        name: "tokio".to_owned(),
        pkg: identifier.clone(),
        dep_kinds: [Kind {
            kind: None,
            target: None,
        }],
    });
    data.resolve
        .as_mut()
        .ok_or_else(|| std::io::Error::other("fixture graph missing"))?
        .nodes
        .push(Node {
            id: identifier,
            dependencies: Vec::new(),
            deps: Vec::new(),
        });
    Ok(data)
}

fn bytes(data: &fixture::Metadata, variant: usize) -> Result<Vec<u8>, std::io::Error> {
    let mut value = serde_json::to_value(data).map_err(std::io::Error::other)?;
    let dependency = value
        .pointer_mut("/packages/3/dependencies/3")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| std::io::Error::other("fixture runtime declaration missing"))?;
    dependency.insert(
        "uses_default_features".to_owned(),
        serde_json::Value::Bool(variant == 3),
    );
    let features = match variant {
        4 => ["net", "rt", "full"],
        _ => ["net", "rt", "time"],
    };
    dependency.insert(
        "features".to_owned(),
        serde_json::Value::Array(
            features
                .into_iter()
                .map(|feature| serde_json::Value::String(feature.to_owned()))
                .collect(),
        ),
    );
    let node = value
        .pointer_mut("/resolve/nodes/4")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| std::io::Error::other("fixture runtime node missing"))?;
    node.insert(
        "features".to_owned(),
        serde_json::Value::Array(
            ["net", "rt", "time", "sync"]
                .into_iter()
                .map(|feature| serde_json::Value::String(feature.to_owned()))
                .collect(),
        ),
    );
    serde_json::to_vec(&value).map_err(std::io::Error::other)
}

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
