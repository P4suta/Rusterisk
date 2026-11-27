use super::{candidate, github, registry, Release};
use std::{collections::BTreeMap, io};

const SOURCE: &str = "0123456789012345678901234567890123456789";

#[test]
fn draft_retries_reuse_matching_assets_and_reject_extra_or_conflicting_assets() -> io::Result<()> {
    let digest = candidate::sha256(b"server");
    let expected = BTreeMap::from([("server.tar.gz".to_owned(), digest.clone())]);
    let asset: github::Asset = serde_json::from_value(serde_json::json!(
        {"id":7,"name":"server.tar.gz","state":"uploaded","digest":format!("sha256:{digest}")}
    ))?;
    let mut assets = Vec::from([asset]);
    assert!(github::missing(&expected, &assets)?.is_empty());
    assert_eq!(github::missing(&expected, &[])?, ["server.tar.gz"]);
    assets[0].state = "starter".into();
    assert!(github::missing(&expected, &assets).is_err());
    assets[0].state = "uploaded".into();
    assets[0].name = "extra.zip".into();
    assert!(github::missing(&expected, &assets).is_err());
    assets[0].name = "server.tar.gz".into();
    assets.push(assets[0].clone());
    assert!(github::missing(&expected, &assets).is_err());
    assets.truncate(1);
    assets[0].digest = Some(format!("sha256:{}", candidate::sha256(b"changed")));
    assert!(github::missing(&expected, &assets).is_err());
    Ok(())
}

#[test]
fn only_a_verified_annotated_tag_can_select_the_supplied_commit() -> io::Result<()> {
    let release = Release::new("0.2.0", SOURCE)?;
    let mut tag = serde_json::json!({"tag":"v0.2.0","object":{"type":"commit","sha":SOURCE},"verification":{"verified":true}});
    assert!(github::verify_tag(&release, &tag).is_ok());
    for (path, bad) in [
        ("/tag", serde_json::json!("v0.1.0")),
        ("/object/type", serde_json::json!("tag")),
        ("/object/sha", serde_json::json!("wrong")),
        ("/verification/verified", serde_json::json!(false)),
    ] {
        let mut copy = tag.clone();
        *copy
            .pointer_mut(path)
            .ok_or_else(|| io::Error::other("test tag field"))? = bad;
        assert!(github::verify_tag(&release, &copy).is_err());
    }
    tag["verification"] = serde_json::Value::Null;
    assert!(github::verify_tag(&release, &tag).is_err());
    Ok(())
}

#[test]
fn registry_receipts_bind_the_immutable_version_bytes_and_github_publisher() -> io::Result<()> {
    let release = Release::new("0.2.0", SOURCE)?;
    let sum = candidate::sha256(b"crate");
    let record = serde_json::json!({"version":{"crate":"rusterisk-cli","num":"0.2.0","checksum":sum,"yanked":false,"trustpub_data":{"provider":"github","repository":"P4suta/Rusterisk","run_id":"123","sha":SOURCE}}});
    assert!(registry::verify_receipt(&release, &sum, &record).is_ok());
    for (path, bad) in [
        ("/version/crate", serde_json::json!("rusterisk")),
        ("/version/num", serde_json::json!("0.1.0")),
        ("/version/checksum", serde_json::json!("other")),
        ("/version/yanked", serde_json::json!(true)),
        ("/version/trustpub_data", serde_json::Value::Null),
        (
            "/version/trustpub_data/repository",
            serde_json::json!("example/Rusterisk"),
        ),
        (
            "/version/trustpub_data/provider",
            serde_json::json!("gitlab"),
        ),
    ] {
        let mut copy = record.clone();
        *copy
            .pointer_mut(path)
            .ok_or_else(|| io::Error::other("test receipt field"))? = bad;
        assert!(registry::verify_receipt(&release, &sum, &copy).is_err());
    }
    Ok(())
}

#[test]
fn registry_request_contains_the_original_archive_and_readme() -> io::Result<()> {
    let release = Release::new("0.2.0", SOURCE)?;
    let archive = b"verified archive";
    let wire = registry::upload_body(&release, "CLI readme", archive)?;
    let mut reader = wire.as_slice();
    use std::io::Read;
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let mut metadata = vec![0; u32::from_le_bytes(size) as usize];
    reader.read_exact(&mut metadata)?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata)?;
    assert_eq!(
        metadata,
        serde_json::json!({"name":"rusterisk-cli","vers":"0.2.0","readme":"CLI readme","readme_file":"README.md"})
    );
    reader.read_exact(&mut size)?;
    assert_eq!(u32::from_le_bytes(size) as usize, archive.len());
    assert_eq!(reader, archive);
    Ok(())
}
