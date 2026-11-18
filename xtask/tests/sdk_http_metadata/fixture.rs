use super::metadata_fixture::{self as fixture, Dependency, Edge, Kind, Node};
use xtask::distribution::{SdkConsumerPlan, SdkConsumerScope, SdkRuntimeIdentity};

pub fn runtime() -> Result<SdkRuntimeIdentity, std::io::Error> {
    let lock = format!("version=4\n[[package]]\nname='tokio'\nversion='1.54.1'\nsource='sparse+opaque-source'\nchecksum='{}'\n", "a".repeat(64));
    SdkRuntimeIdentity::from_lock(lock.as_bytes()).map_err(std::io::Error::other)
}

pub fn data(plan: &SdkConsumerPlan) -> Result<fixture::Metadata, std::io::Error> {
    let runtime = runtime()?;
    with_runtime(plan, &runtime)
}

pub fn with_runtime(
    plan: &SdkConsumerPlan,
    runtime: &SdkRuntimeIdentity,
) -> Result<fixture::Metadata, std::io::Error> {
    let mut data = fixture::metadata(plan, SdkConsumerScope::Sdk);
    let mut tokio = fixture::external(plan, 0);
    tokio.name = "tokio".to_owned();
    tokio.id = "opaque-package:tokio".to_owned();
    tokio.version = runtime.version().to_owned();
    tokio.source = Some(runtime.source().to_owned());
    let identifier = tokio.id.clone();
    data.packages.push(tokio);
    data.package(SdkConsumerScope::Sdk.package_name())?
        .dependencies
        .push(Dependency {
            name: "tokio".to_owned(),
            req: format!("={}", runtime.version()),
            source: Some(runtime.source().to_owned()),
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

pub fn bytes(data: &fixture::Metadata, variant: usize) -> Result<Vec<u8>, std::io::Error> {
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
