#[test]
fn isolated_advisory_configuration_preserves_every_policy_value(
) -> Result<(), xtask::SecurityConfigError> {
    let source = include_str!("../../deny.toml");
    let original: toml::Value = toml::from_str(source)?;
    for path in ["/snapshot/advisories", r"C:\cache\advisories"] {
        let mut isolated = xtask::advisory_snapshot_config(source, path)?;
        assert_eq!(isolated["advisories"]["db-path"].as_str(), Some(path));
        if let Some(advisories) = isolated
            .get_mut("advisories")
            .and_then(toml::Value::as_table_mut)
        {
            advisories.remove("db-path");
        }
        assert_eq!(isolated, original);
    }
    Ok(())
}

#[test]
fn refuses_a_missing_or_malformed_advisory_policy() {
    for source in ["", "[advisories", "advisories = 1"] {
        assert!(xtask::advisory_snapshot_config(source, "/snapshot").is_err());
    }
}
