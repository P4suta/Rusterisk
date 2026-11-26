use serde::Deserialize;

#[derive(Deserialize)]
struct ReleaseConfig {
    workspace: TagSelection,
    package: Vec<TagSelection>,
}

#[derive(Deserialize)]
struct TagSelection {
    name: Option<String>,
    #[serde(default)]
    git_only: bool,
}

#[test]
fn only_the_cli_uses_the_shared_release_tag() -> Result<(), toml::de::Error> {
    let config: ReleaseConfig = toml::from_str(include_str!("../../release-plz.toml"))?;
    assert!(
        !config.workspace.git_only,
        "new workspace members must not inherit the CLI release tag"
    );
    let tagged: Vec<_> = config
        .package
        .iter()
        .filter(|package| package.git_only)
        .map(|package| package.name.as_deref())
        .collect();
    assert_eq!(tagged, [Some("rusterisk-cli")]);
    Ok(())
}
