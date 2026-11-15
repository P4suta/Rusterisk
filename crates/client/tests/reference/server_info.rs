use rusterisk_client::{
    api::{AsteriskInfo, InfoSection},
    Client, Error,
};

fn sections(info: &AsteriskInfo) -> [bool; 4] {
    [
        info.build.is_some(),
        info.system.is_some(),
        info.config.is_some(),
        info.status.is_some(),
    ]
}

pub(super) async fn compare(
    actual: &Client,
    actual_url: &str,
    reference: &Client,
    reference_url: &str,
) -> Result<(), Error> {
    for mask in 0..16 {
        let selected: Vec<_> = InfoSection::ALL
            .into_iter()
            .enumerate()
            .filter_map(|(index, section)| (mask & (1 << index) != 0).then_some(section))
            .collect();
        let actual = actual.server_info(&selected).await?;
        let reference = reference.server_info(&selected).await?;
        assert_eq!(
            sections(&actual),
            sections(&reference),
            "section mask {mask}"
        );
    }
    let raw = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    for only in ["SYSTEM,status,unknown", "unknown", "", "build,build"] {
        let mut observed = Vec::new();
        for base in [actual_url, reference_url] {
            let mut url = reqwest::Url::parse(&format!("{base}/ari/asterisk/info"))
                .map_err(|_| Error::Invalid("reference information URL"))?;
            url.query_pairs_mut().append_pair("only", only);
            let response = raw
                .get(url)
                .basic_auth("app", Some("test-password"))
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK, "{only}");
            let info: AsteriskInfo = response.json().await?;
            observed.push(sections(&info));
        }
        assert_eq!(observed[0], observed[1], "only={only}");
    }
    Ok(())
}
