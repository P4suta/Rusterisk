use rusterisk_client::{
    api::{Bridge, BridgeVideoMode, Channel, ChannelId, MediaControlFormat, MediaEvent},
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

pub async fn initial_variables(
    client: &Client,
    address: &str,
    application: &str,
) -> Result<(), Error> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut url = reqwest::Url::parse(&format!("{address}/ari/channels/externalMedia"))
        .map_err(|_| Error::Invalid("reference address"))?;
    url.query_pairs_mut().extend_pairs([
        ("channelId", "json-variables"),
        ("app", application),
        ("external_host", "127.0.0.1:12000"),
        ("format", "ulaw"),
    ]);
    let response = http.post(url)
        .basic_auth("app", Some("test-password"))
        .json(&serde_json::json!({"variables": {"FIRST": "value /&=test", "EMPTY": "", "CALLERID(name)": "Alice"}}))
        .send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let channel: Channel = response.json().await?;
    assert_eq!(channel.caller.name, "Alice");
    let variables = channel
        .channelvars
        .as_ref()
        .ok_or(Error::Invalid("initial channel variables"))?;
    assert_eq!(
        variables.get("FIRST").map(String::as_str),
        Some("value /&=test")
    );
    assert_eq!(variables.get("EMPTY").map(String::as_str), Some(""));
    for (name, expected) in [
        ("FIRST", "value /&=test"),
        ("EMPTY", ""),
        ("CALLERID(name)", "Alice"),
    ] {
        assert_eq!(client.variable(&channel.id, name).await?.value, expected);
    }
    client.hangup(&channel.id).await
}

pub async fn originate_with_id(
    client: &Client,
    address: &str,
    application: &str,
) -> Result<(), Error> {
    let id = ChannelId::try_from("path-originated".to_owned())?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let operation = async {
        let response = http
            .post(format!(
                "{address}/ari/channels/{id}?channelId=query-originated"
            ))
            .basic_auth("app", Some("test-password"))
            .json(&serde_json::json!({
                "endpoint":"PJSIP/phone-extra",
                "app":application,
                "channelId":"body-originated",
                "callerId":"Path caller <101>",
                "timeout":2,
                "variables":{"INITIAL_TAG":"path value"}
            }))
            .send()
            .await?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Error::Invalid("originateWithId admission response"));
        }
        let channel: Channel = response.json().await?;
        if channel.id != id
            || channel.caller.number != "101"
            || channel.caller.name != "Path caller"
        {
            return Err(Error::Invalid("originateWithId channel identity"));
        }
        Ok(())
    }
    .await;
    let cleanup = match client.hangup(&id).await {
        Ok(()) | Err(Error::Api { status: 404, .. }) => Ok(()),
        Err(error) => Err(error),
    };
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{operation}; originate cleanup failed: {cleanup}"
        ))),
    }
}

pub async fn websocket_variables(
    client: &Client,
    address: &str,
    application: &str,
) -> Result<(), Error> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut url = reqwest::Url::parse(&format!("{address}/ari/channels/externalMedia"))
        .map_err(|_| Error::Invalid("reference address"))?;
    url.query_pairs_mut().extend_pairs([
        ("channelId", "json-websocket-variables"),
        ("app", application),
        ("format", "ulaw"),
        ("encapsulation", "none"),
        ("transport", "websocket"),
        ("connection_type", "server"),
        ("transport_data", "f(json)"),
    ]);
    let response = http
        .post(url)
        .basic_auth("app", Some("test-password"))
        .json(&serde_json::json!({"variables":{"INITIAL_TAG":"media value /&=test"}}))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let channel: Channel = response.json().await?;
    let media = client
        .connect_media(&channel, MediaControlFormat::Json)
        .await?;
    let MediaEvent::MediaStart {
        channel_variables, ..
    } = media.start_event()
    else {
        return Err(Error::Invalid("media start event"));
    };
    assert_eq!(
        channel_variables.get("INITIAL_TAG").map(String::as_str),
        Some("media value /&=test")
    );
    assert!(channel_variables.contains_key("MEDIA_WEBSOCKET_CONNECTION_ID"));
    media.close().await?;
    match client.hangup(&channel.id).await {
        Ok(()) | Err(Error::Api { status: 404, .. }) => Ok(()),
        Err(error) => Err(error),
    }
}
