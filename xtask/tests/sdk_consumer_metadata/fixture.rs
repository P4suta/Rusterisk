use serde::Serialize;
use std::path::PathBuf;
use xtask::distribution::{SdkConsumerPlan, SdkConsumerScope, SdkPackage};

#[derive(Clone, Serialize)]
pub struct Metadata {
    pub version: u8,
    pub packages: Vec<Package>,
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    pub workspace_members: [String; 1],
    pub resolve: Option<Resolve>,
}

#[derive(Clone, Serialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub manifest_path: PathBuf,
    pub dependencies: Vec<Dependency>,
    pub targets: [Target; 1],
}

#[derive(Clone, Serialize)]
pub struct Dependency {
    pub name: String,
    pub req: String,
    pub source: Option<String>,
    pub path: Option<PathBuf>,
    pub kind: Option<String>,
    pub target: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct Target {
    pub kind: [String; 1],
    pub src_path: PathBuf,
}

#[derive(Clone, Serialize)]
pub struct Resolve {
    pub root: Option<String>,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Serialize)]
pub struct Node {
    pub id: String,
    pub dependencies: Vec<String>,
    pub deps: Vec<Edge>,
}

#[derive(Clone, Serialize)]
pub struct Edge {
    pub name: String,
    pub pkg: String,
    pub dep_kinds: [Kind; 1],
}

#[derive(Clone, Serialize)]
pub struct Kind {
    pub kind: Option<String>,
    pub target: Option<String>,
}

pub fn plan() -> Result<SdkConsumerPlan, std::io::Error> {
    SdkConsumerPlan::new(
        &std::env::temp_dir().join("rusterisk-sdk-metadata-work"),
        &std::env::temp_dir().join("rusterisk-sdk-metadata-state"),
    )
}

fn id(name: &str) -> String {
    format!("opaque-package:{name}")
}

fn dependency(name: &str) -> Dependency {
    Dependency {
        name: name.to_owned(),
        req: "=0.1.0".to_owned(),
        source: Some("registry+opaque-source".to_owned()),
        path: None,
        kind: None,
        target: None,
    }
}

fn node(name: &str, dependencies: &[&str]) -> Node {
    Node {
        id: id(name),
        dependencies: dependencies.iter().map(|name| id(name)).collect(),
        deps: dependencies
            .iter()
            .map(|name| Edge {
                name: name.replace('-', "_"),
                pkg: id(name),
                dep_kinds: [Kind {
                    kind: None,
                    target: None,
                }],
            })
            .collect(),
    }
}

pub fn external(plan: &SdkConsumerPlan, index: usize) -> Package {
    let name = format!("registry-package-{index}");
    Package {
        id: id(&name),
        name: name.clone(),
        version: "1.0.0".to_owned(),
        source: Some("sparse+opaque-source".to_owned()),
        manifest_path: plan.state().join("registry").join(&name).join("Cargo.toml"),
        dependencies: Vec::new(),
        targets: [Target {
            kind: ["lib".to_owned()],
            src_path: plan.state().join("registry").join(name).join("src/lib.rs"),
        }],
    }
}

pub fn metadata(plan: &SdkConsumerPlan, scope: SdkConsumerScope) -> Metadata {
    let selected: &[SdkPackage] = match scope {
        SdkConsumerScope::CoreMsrv => &[SdkPackage::Core],
        SdkConsumerScope::Sdk => &SdkPackage::ALL,
    };
    let mut packages = Vec::new();
    let mut nodes = Vec::new();
    for package in selected {
        let directory = plan
            .work()
            .join("packages")
            .join(format!("{}-0.1.0", package.name()));
        let deps: &[&str] = if *package == SdkPackage::Client {
            &["rusterisk-core", "rusterisk-api"]
        } else {
            &[]
        };
        packages.push(Package {
            id: id(package.name()),
            name: package.name().to_owned(),
            version: "0.1.0".to_owned(),
            source: None,
            manifest_path: directory.join("Cargo.toml"),
            dependencies: deps.iter().map(|name| dependency(name)).collect(),
            targets: [Target {
                kind: ["lib".to_owned()],
                src_path: directory.join("src/lib.rs"),
            }],
        });
        nodes.push(node(package.name(), deps));
    }
    let names: Vec<_> = selected.iter().map(|package| package.name()).collect();
    packages.push(Package {
        id: id(scope.package_name()),
        name: scope.package_name().to_owned(),
        version: "0.0.0".to_owned(),
        source: None,
        manifest_path: plan.directory(scope).join("Cargo.toml"),
        dependencies: names.iter().map(|name| dependency(name)).collect(),
        targets: [Target {
            kind: ["bin".to_owned()],
            src_path: plan.directory(scope).join("src/main.rs"),
        }],
    });
    nodes.push(node(scope.package_name(), &names));
    Metadata {
        version: 1,
        packages,
        workspace_root: plan.directory(scope).to_owned(),
        target_directory: plan.target(scope).to_owned(),
        workspace_members: [id(scope.package_name())],
        resolve: Some(Resolve {
            root: Some(id(scope.package_name())),
            nodes,
        }),
    }
}

impl Metadata {
    pub fn bytes(&self) -> Result<Vec<u8>, std::io::Error> {
        serde_json::to_vec(self).map_err(std::io::Error::other)
    }
    pub fn package(&mut self, name: &str) -> Result<&mut Package, std::io::Error> {
        self.packages
            .iter_mut()
            .find(|package| package.name == name)
            .ok_or_else(|| std::io::Error::other("fixture package missing"))
    }
    pub fn node(&mut self, name: &str) -> Result<&mut Node, std::io::Error> {
        self.resolve
            .as_mut()
            .and_then(|resolve| resolve.nodes.iter_mut().find(|node| node.id == id(name)))
            .ok_or_else(|| std::io::Error::other("fixture graph node missing"))
    }
    pub fn add_external(
        &mut self,
        plan: &SdkConsumerPlan,
        index: usize,
    ) -> Result<(), std::io::Error> {
        let package = external(plan, index);
        self.resolve
            .as_mut()
            .ok_or_else(|| std::io::Error::other("fixture resolve missing"))?
            .nodes
            .push(Node {
                id: package.id.clone(),
                dependencies: Vec::new(),
                deps: Vec::new(),
            });
        self.packages.push(package);
        Ok(())
    }
}

pub fn checkout(plan: &SdkConsumerPlan) -> PathBuf {
    plan.work().with_file_name("checkout")
}
