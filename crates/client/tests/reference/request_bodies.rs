use rusterisk_client::{
    api::{Bridge, BridgeVideoMode},
    Client, Error,
};
use std::time::Duration;

pub async fn compare(
    actual: &Client,
    actual_address: &str,
    reference: &Client,
    reference_address: &str,
) -> Result<(), Error> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    for (path, body, id, name) in [
        (
            "/ari/bridges?bridgeId=query&name=query",
            serde_json::json!({"bridgeId":"json-body", "name":"Body name", "type":"mixing", "unknown":{"ignored":true}}),
            "json-body",
            "Body name",
        ),
        (
            "/ari/bridges/json-path?name=query",
            serde_json::json!({"bridgeId":"intruder", "name":"Path name", "type":"mixing"}),
            "json-path",
            "Path name",
        ),
        (
            "/ari/bridges/json-cleared?name=query",
            serde_json::json!({"name":false}),
            "json-cleared",
            "",
        ),
    ] {
        let mut snapshots = Vec::with_capacity(2);
        for (client, address, mode) in [
            (actual, actual_address, BridgeVideoMode::None),
            (reference, reference_address, BridgeVideoMode::Talker),
        ] {
            let response = http
                .post(format!("{address}{path}"))
                .basic_auth("app", Some("test-password"))
                .json(&body)
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
            let bridge: Bridge = response.json().await?;
            assert_eq!(bridge.id.as_str(), id);
            assert_eq!(bridge.name, name);
            client.destroy_bridge(&bridge.id).await?;
            snapshots.push(super::audio_bridge(bridge, mode));
        }
        assert_eq!(snapshots[0], snapshots[1]);
    }
    Ok(())
}
