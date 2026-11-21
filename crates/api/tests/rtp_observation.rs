use rusterisk_api::openapi;

#[test]
fn live_rtp_statistics_bind_documented_server_and_peer_measurements_to_one_channel() {
    let api = openapi();
    let path = &api["paths"]["/rusterisk/channels/{channelId}/rtp-statistics"];
    assert_eq!(path["servers"][0]["url"], "/");
    let get = &path["get"];
    assert_eq!(get["operationId"], "channels.rtpObservation");
    assert_eq!(get["parameters"][0]["name"], "channelId");
    assert_eq!(get["parameters"][0]["in"], "path");
    assert_eq!(
        get["security"][0]["basicAuth"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        get["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/RtpObservation"
    );
    let fields = &api["components"]["schemas"]["RtpObservation"]["properties"];
    for field in [
        "channel_id",
        "generation",
        "creationtime",
        "observed_at",
        "local",
        "peer",
    ] {
        assert!(fields.get(field).is_some(), "missing {field}");
    }
}
