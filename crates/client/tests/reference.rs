use rusterisk_client::{
    api::{
        ApiError, BridgeVideoMode, ChannelId, EventFilter, EventFilterRequest, EventKind,
        EventSource, ExternalRtpTarget, StaticRtpCodec,
    },
    Bridge, Client, Error,
};
use rusterisk_server::test_time::Wait;
use std::net::Ipv4Addr;
#[path = "reference/ari_routes.rs"]
mod ari_routes;
#[path = "reference/dialplan_routes.rs"]
mod dialplan_routes;
#[path = "reference/external_audio.rs"]
mod external_audio;
#[path = "reference/external_opus.rs"]
mod external_opus;
#[path = "reference/external_streams.rs"]
mod external_streams;
#[path = "reference/external_websocket_clients.rs"]
mod external_websocket_clients;
#[path = "reference/external_websockets.rs"]
mod external_websockets;
#[path = "home_telephone_media/scenario.rs"]
mod home_telephone_media;
#[path = "reference/music_on_hold.rs"]
mod music_on_hold;
#[path = "reference/playback_position.rs"]
mod playback_position;
#[path = "support/received_audio.rs"]
mod received_audio;
#[path = "reference/recording_audio.rs"]
mod recording_audio;
#[path = "reference/request_bodies.rs"]
mod request_bodies;
#[path = "reference/runtime.rs"]
mod runtime;
#[path = "reference/server_info.rs"]
mod server_info;
#[path = "reference/sound_playback.rs"]
mod sound_playback;
#[path = "reference/sounds.rs"]
mod sounds;
#[path = "recording_audio/wave.rs"]
mod wave;

async fn compare_external_admission(
    client: &Client,
    session: &mut rusterisk_client::Session,
    app: &str,
) -> Result<(), Error> {
    let peer = rusterisk_server::rtp::SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0)?;
    let id = ChannelId::try_from("external-compat".to_owned())?;
    let target = ExternalRtpTarget::new(peer.local_addr()?, StaticRtpCodec::Pcmu)
        .map_err(|_| Error::Invalid("external peer"))?;
    let channel = client.external_rtp(&id, app, target, "media,agent").await?;
    assert_eq!(channel.id, id);
    assert!(channel.name.starts_with("UnicastRTP/"));
    let variables = channel
        .channelvars
        .ok_or(Error::Invalid("transport variables"))?;
    assert!(variables
        .get("UNICASTRTP_LOCAL_ADDRESS")
        .is_some_and(|value| value
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| !ip.is_unspecified())));
    assert!(variables
        .get("UNICASTRTP_LOCAL_PORT")
        .is_some_and(|value| value
            .parse::<u16>()
            .is_ok_and(|port| port > 0 && port < u16::MAX)));
    Wait::Ready
        .bounded(async {
            for _ in 0..32 {
                if let EventKind::StasisStart { channel, args } = session.next_event().await?.kind {
                    if channel.id == id {
                        assert_eq!(args, ["media", "agent"]);
                        return Ok(());
                    }
                }
            }
            Err(Error::Capacity)
        })
        .await
        .map_err(|_| Error::Timeout)??;
    client.hangup(&id).await
}

fn audio_bridge(mut bridge: Bridge, expected_mode: BridgeVideoMode) -> Bridge {
    assert_eq!(bridge.video_mode, Some(expected_mode));
    assert!(bridge.video_source_id.is_none());
    assert!(!bridge.creationtime.is_empty());
    bridge.creationtime.clear();
    bridge.video_mode = None;
    bridge
}

async fn compare_bridges(actual: &Client, reference: &Client) -> Result<(), Error> {
    let id = rusterisk_client::BridgeId::try_from("compat-bridge".to_owned())?;
    let created = actual.create_bridge(&id, "Conversation").await?;
    let expected = reference.create_bridge(&id, "Conversation").await?;
    assert_eq!(
        audio_bridge(created, BridgeVideoMode::None),
        audio_bridge(expected, BridgeVideoMode::Talker)
    );
    assert_eq!(
        audio_bridge(actual.bridge(&id).await?, BridgeVideoMode::None),
        audio_bridge(reference.bridge(&id).await?, BridgeVideoMode::Talker)
    );
    let actual_list: Vec<Bridge> = actual
        .bridges()
        .await?
        .into_iter()
        .map(|bridge| audio_bridge(bridge, BridgeVideoMode::None))
        .collect();
    let expected_list: Vec<Bridge> = reference
        .bridges()
        .await?
        .into_iter()
        .map(|bridge| audio_bridge(bridge, BridgeVideoMode::Talker))
        .collect();
    assert_eq!(actual_list, expected_list);
    assert_eq!(
        rejection(actual.create_bridge(&id, "Duplicate").await)?,
        rejection(reference.create_bridge(&id, "Duplicate").await)?
    );
    actual.destroy_bridge(&id).await?;
    reference.destroy_bridge(&id).await?;
    assert_eq!(
        rejection(actual.bridge(&id).await)?,
        rejection(reference.bridge(&id).await)?
    );
    assert_eq!(
        rejection(actual.destroy_bridge(&id).await)?,
        rejection(reference.destroy_bridge(&id).await)?
    );
    Ok(())
}

fn rejection<T>(result: Result<T, Error>) -> Result<(u16, ApiError), Error> {
    match result {
        Err(Error::Api { status, message }) => Ok((status, message)),
        Err(error) => Err(error),
        Ok(_) => Err(Error::Invalid("expected a rejected resource query")),
    }
}

async fn compare_subscriptions(
    actual: &Client,
    reference: &Client,
    app: &str,
) -> Result<(), Error> {
    let id = rusterisk_client::BridgeId::try_from("observed".to_owned())?;
    for text in ["endpoint:PJSIP/phone", "endpoint:PJSIP"] {
        let endpoint =
            EventSource::try_from(text).map_err(|_| Error::Invalid("endpoint source"))?;
        assert_eq!(
            actual
                .subscribe_resources(app, std::slice::from_ref(&endpoint))
                .await?,
            reference
                .subscribe_resources(app, std::slice::from_ref(&endpoint))
                .await?
        );
        assert_eq!(
            actual
                .unsubscribe_resources(app, std::slice::from_ref(&endpoint))
                .await?,
            reference
                .unsubscribe_resources(app, std::slice::from_ref(&endpoint))
                .await?
        );
    }
    let unknown = EventSource::try_from("endpoint:pjsip/phone")
        .map_err(|_| Error::Invalid("endpoint source"))?;
    assert_eq!(
        rejection(
            actual
                .subscribe_resources(app, std::slice::from_ref(&unknown))
                .await
        )?,
        rejection(reference.subscribe_resources(app, &[unknown]).await)?
    );
    actual.create_bridge(&id, "Observed").await?;
    reference.create_bridge(&id, "Observed").await?;
    let source = EventSource::from(id.clone());
    for _ in 0..2 {
        assert_eq!(
            actual
                .subscribe_resources(app, std::slice::from_ref(&source))
                .await?,
            reference
                .subscribe_resources(app, std::slice::from_ref(&source))
                .await?
        );
    }
    for _ in 0..2 {
        assert_eq!(
            actual
                .unsubscribe_resources(app, std::slice::from_ref(&source))
                .await?,
            reference
                .unsubscribe_resources(app, std::slice::from_ref(&source))
                .await?
        );
    }
    assert_eq!(
        rejection(
            actual
                .unsubscribe_resources(app, std::slice::from_ref(&source))
                .await
        )?,
        rejection(
            reference
                .unsubscribe_resources(app, std::slice::from_ref(&source))
                .await
        )?
    );
    let missing = EventSource::Bridge(rusterisk_client::BridgeId::try_from("missing".to_owned())?);
    let batch = [source.clone(), missing];
    assert_eq!(
        rejection(actual.subscribe_resources(app, &batch).await)?,
        rejection(reference.subscribe_resources(app, &batch).await)?
    );
    let mut released = actual.application(app).await?;
    assert_eq!(released, reference.application(app).await?);
    assert_eq!(released.bridge_ids.as_slice(), std::slice::from_ref(&id));
    released.bridge_ids.clear();
    actual.destroy_bridge(&id).await?;
    reference.destroy_bridge(&id).await?;
    assert_eq!(actual.application(app).await?, released);
    // Asterisk unsubscribes in its bridge snapshot callback.
    let reference_released = Wait::Ready
        .bounded(async {
            loop {
                let application = reference.application(app).await?;
                if !application.bridge_ids.contains(&id) {
                    return Ok::<_, Error>(application);
                }
            }
        })
        .await
        .map_err(|_| Error::Invalid("reference bridge subscription cleanup deadline"))??;
    assert_eq!(reference_released, released);
    assert_eq!(
        rejection(
            actual
                .unsubscribe_resources(app, std::slice::from_ref(&source))
                .await
        )?,
        rejection(reference.unsubscribe_resources(app, &[source]).await)?
    );
    assert_eq!(
        actual
            .subscribe_resources(app, &[EventSource::AllBridges])
            .await?,
        reference
            .subscribe_resources(app, &[EventSource::AllBridges])
            .await?
    );
    let cleared = actual
        .unsubscribe_resources(app, &[EventSource::AllBridges])
        .await?;
    assert!(cleared.bridge_ids.is_empty());
    assert_eq!(
        rejection(
            reference
                .unsubscribe_resources(app, &[EventSource::AllBridges])
                .await
        )?,
        (
            501,
            ApiError {
                message: "Method not implemented".into()
            }
        )
    );
    Ok(())
}

async fn compare_existing_client(actual: &str, reference: &str) -> Result<(), Error> {
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/interop/node-ari-client/scenario.ts");
    let mut child = tokio::process::Command::new("node")
        .arg("--max-old-space-size=128")
        .arg(scenario)
        .env("RUSTERISK_ACTUAL_URL", actual)
        .env("RUSTERISK_REFERENCE_URL", reference)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let status = match Wait::Shutdown.bounded(child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            child.kill().await?;
            return Err(Error::Timeout);
        }
    };
    if status.success() {
        println!("Existing client resource comparison passed.");
        Ok(())
    } else {
        eprintln!("Existing client comparison failed: {status}");
        Err(Error::Invalid("existing client comparison failed"))
    }
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn resource_queries_and_app_subscriptions_match_the_reference() -> Result<(), Error> {
    let reference_address = std::env::var("RUSTERISK_REFERENCE_URL")
        .map_err(|_| Error::Invalid("reference URL is required"))?;
    let reference = Client::new(&reference_address, "app", "test-password")?;
    Wait::Shutdown
        .bounded(async {
            loop {
                if reference.ping().await.is_ok() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)?;
    let mut outgoing = external_websocket_clients::Fixture::start().await?;
    let result = compare_resources(reference_address, reference, &mut outgoing).await;
    let cleanup = outgoing.shutdown().await;
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Err(cleanup)) => {
            eprintln!("Reference comparison failed: {primary}; callback cleanup failed: {cleanup}");
            Err(primary)
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
    }
}

async fn compare_resources(
    reference_address: String,
    reference: Client,
    outgoing: &mut external_websocket_clients::Fixture,
) -> Result<(), Error> {
    outgoing.configure_reference(&reference_address).await?;
    let mut configuration = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        include_str!("../../../tests/interop/asterisk/ari.conf"),
        &include_str!("../../../tests/interop/asterisk/pjsip.conf")
            .replace("0.0.0.0:5060", "127.0.0.1:0"),
        "",
        include_str!("../../../tests/interop/asterisk/extensions.conf"),
    )
    .map_err(|_| Error::Invalid("reference configuration"))?
    .with_websocket_configuration("", &outgoing.configuration("127.0.0.1"))
    .map_err(|_| Error::Invalid("native WebSocket configuration"))?;
    configuration.http.bind.set_port(0);
    let fixture_root = std::env::var("RUSTERISK_REFERENCE_CONFIGURATION")
        .map_err(|_| Error::Invalid("reference configuration directory is required"))?;
    let fixture =
        runtime::Fixture::start(configuration, std::path::Path::new(&fixture_root)).await?;
    let address = fixture.address();
    let actual = fixture.client();
    let result = async {
        ari_routes::compare(&format!("http://{address}"), &reference_address).await?;
        server_info::compare(
            actual,
            &format!("http://{address}"),
            &reference,
            &reference_address,
        )
        .await?;
        sounds::compare(actual, &reference).await?;
        assert_eq!(actual.channels().await?, reference.channels().await?);
        assert_eq!(
            actual.applications().await?,
            reference.applications().await?
        );
        compare_bridges(actual, &reference).await?;
        request_bodies::compare(
            actual,
            &format!("http://{address}"),
            &reference,
            &reference_address,
        )
        .await?;
        let mut expected = reference.endpoints().await?;
        expected.sort_by(|left, right| left.resource.cmp(&right.resource));
        assert_eq!(actual.endpoints().await?, expected);
        for name in ["phone", "phone-extra"] {
            assert_eq!(
                actual.endpoint("PJSIP", name).await?,
                reference.endpoint("PJSIP", name).await?
            );
        }
        for technology in ["PJSIP", "pjsip", "Pjsip"] {
            match reference.endpoints_by_technology(technology).await {
                Ok(mut expected) => {
                    expected.sort_by(|left, right| left.resource.cmp(&right.resource));
                    assert_eq!(actual.endpoints_by_technology(technology).await?, expected);
                }
                Err(Error::Api { status, message }) => {
                    assert_eq!(
                        rejection(actual.endpoints_by_technology(technology).await)?,
                        (status, message)
                    );
                }
                Err(error) => return Err(error),
            }
            match reference.endpoint(technology, "phone").await {
                Ok(expected) => assert_eq!(actual.endpoint(technology, "phone").await?, expected),
                Err(Error::Api { status, message }) => {
                    assert_eq!(
                        rejection(actual.endpoint(technology, "phone").await)?,
                        (status, message)
                    );
                }
                Err(error) => return Err(error),
            }
        }
        assert_eq!(
            rejection(actual.endpoint("PJSIP", "missing").await)?,
            rejection(reference.endpoint("PJSIP", "missing").await)?
        );
        assert_eq!(
            rejection(actual.endpoints_by_technology("unknown").await)?,
            rejection(reference.endpoints_by_technology("unknown").await)?
        );
        let app = ChannelId::try_from("compat-app".to_owned())?;
        let mut actual_session = actual.subscribe(&app).await?;
        let mut reference_session = reference.subscribe(&app).await?;
        assert_eq!(
            actual.application(app.as_str()).await?,
            reference.application(app.as_str()).await?
        );
        for update in [
            EventFilterRequest {
                allowed: Some(
                    [EventFilter {
                        event_type: "StasisStart".into(),
                    }]
                    .into_iter()
                    .collect(),
                ),
                disallowed: Some(
                    [EventFilter {
                        event_type: "ChannelVarset".into(),
                    }]
                    .into_iter()
                    .collect(),
                ),
            },
            EventFilterRequest {
                allowed: Some(Vec::new()),
                disallowed: None,
            },
            EventFilterRequest::default(),
        ] {
            assert_eq!(
                actual.filter_application(app.as_str(), &update).await?,
                reference.filter_application(app.as_str(), &update).await?
            );
        }
        let invalid = EventFilterRequest {
            allowed: Some(
                [EventFilter {
                    event_type: String::new(),
                }]
                .into_iter()
                .collect(),
            ),
            disallowed: None,
        };
        assert_eq!(
            rejection(actual.filter_application(app.as_str(), &invalid).await)?,
            rejection(reference.filter_application(app.as_str(), &invalid).await)?
        );
        assert_eq!(
            rejection(
                actual
                    .filter_application("missing", &Default::default())
                    .await
            )?,
            rejection(
                reference
                    .filter_application("missing", &Default::default())
                    .await
            )?
        );
        assert_eq!(
            rejection(actual.application("missing").await)?,
            rejection(reference.application("missing").await)?
        );
        compare_external_admission(actual, &mut actual_session, app.as_str()).await?;
        compare_external_admission(&reference, &mut reference_session, app.as_str()).await?;
        dialplan_routes::compare(&reference, &mut reference_session, app.as_str()).await?;
        dialplan_routes::compare(actual, &mut actual_session, app.as_str()).await?;
        request_bodies::originate_with_id(actual, &format!("http://{address}"), app.as_str())
            .await?;
        request_bodies::originate_with_id(&reference, &reference_address, app.as_str()).await?;
        request_bodies::initial_variables(actual, &format!("http://{address}"), app.as_str())
            .await?;
        request_bodies::initial_variables(&reference, &reference_address, app.as_str()).await?;
        request_bodies::websocket_variables(actual, &format!("http://{address}"), app.as_str())
            .await?;
        request_bodies::websocket_variables(&reference, &reference_address, app.as_str()).await?;
        external_audio::compare(
            actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        let reference_media_ip = std::env::var("RUSTERISK_REFERENCE_MEDIA_IP")
            .map_err(|_| Error::Invalid("reference media address is required"))?
            .parse::<Ipv4Addr>()
            .map_err(|_| Error::Invalid("reference media address"))?;
        external_audio::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            reference_media_ip,
        )
        .await?;
        sound_playback::compare(
            actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        music_on_hold::compare(
            actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
            music_on_hold::ComparisonServer::Rusterisk,
        )
        .await?;
        playback_position::compare(
            actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        recording_audio::compare(
            actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        external_streams::compare(
            actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        let callback_ip = std::env::var("RUSTERISK_REFERENCE_CALLBACK_IP")
            .map_err(|_| Error::Invalid("reference callback address is required"))?
            .parse::<Ipv4Addr>()
            .map_err(|_| Error::Invalid("reference callback address"))?;
        sound_playback::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            callback_ip,
        )
        .await?;
        music_on_hold::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            reference_media_ip,
            music_on_hold::ComparisonServer::Asterisk,
        )
        .await?;
        playback_position::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            callback_ip,
        )
        .await?;
        recording_audio::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            callback_ip,
        )
        .await?;
        external_streams::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            callback_ip,
        )
        .await?;
        external_websockets::compare(
            &reference,
            &reference_address,
            &mut reference_session,
            app.as_str(),
        )
        .await?;
        external_websockets::compare(
            actual,
            &format!("http://{address}"),
            &mut actual_session,
            app.as_str(),
        )
        .await?;
        home_telephone_media::compare(&reference_address).await?;
        home_telephone_media::compare(&format!("http://{address}")).await?;
        external_opus::compare(
            &reference,
            &reference_address,
            &mut reference_session,
            app.as_str(),
        )
        .await?;
        external_opus::compare(
            actual,
            &format!("http://{address}"),
            &mut actual_session,
            app.as_str(),
        )
        .await?;
        external_websocket_clients::compare(
            &reference,
            &mut reference_session,
            app.as_str(),
            outgoing,
        )
        .await?;
        external_websocket_clients::compare(actual, &mut actual_session, app.as_str(), outgoing)
            .await?;
        compare_subscriptions(actual, &reference, app.as_str()).await?;
        reference_session.shutdown().await?;
        actual_session.shutdown().await?;
        compare_existing_client(&format!("http://{address}"), &reference_address).await?;
        Ok(())
    }
    .await;
    fixture.finish(result).await
}
