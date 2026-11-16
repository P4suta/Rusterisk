use rusterisk_client::{
    api::{
        ApiError, Bridge, BridgeVideoMode, ChannelId, EventFilter, EventFilterRequest, EventKind,
        EventSource, ExternalRtpTarget, StaticRtpCodec,
    },
    Client, Error,
};
use std::{future::IntoFuture, net::Ipv4Addr, time::Duration};
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
#[path = "reference/playback_position.rs"]
mod playback_position;
#[path = "reference/recording_audio.rs"]
mod recording_audio;
#[path = "reference/request_bodies.rs"]
mod request_bodies;
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
    tokio::time::timeout(Duration::from_secs(3), async {
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
    let id = ChannelId::try_from("compat-bridge".to_owned())?;
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
    let id = ChannelId::try_from("observed".to_owned())?;
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
    let source = EventSource::Bridge(id.clone());
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
    let missing = EventSource::Bridge(ChannelId::try_from("missing".to_owned())?);
    let batch = [source.clone(), missing];
    assert_eq!(
        rejection(actual.subscribe_resources(app, &batch).await)?,
        rejection(reference.subscribe_resources(app, &batch).await)?
    );
    assert_eq!(
        actual.application(app).await?,
        reference.application(app).await?
    );
    actual.destroy_bridge(&id).await?;
    reference.destroy_bridge(&id).await?;
    assert_eq!(
        actual.application(app).await?,
        reference.application(app).await?
    );
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
        .kill_on_drop(true)
        .spawn()?;
    let status = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            child.kill().await?;
            return Err(Error::Timeout);
        }
    };
    if status.success() {
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
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if reference.ping().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
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
    let configuration = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &include_str!("../../../tests/interop/asterisk/pjsip.conf")
            .replace("0.0.0.0:5060", "127.0.0.1:0"),
        "",
        "",
    )
    .map_err(|_| Error::Invalid("reference configuration"))?
    .with_websocket_configuration("", &outgoing.configuration("127.0.0.1"))
    .map_err(|_| Error::Invalid("native WebSocket configuration"))?;
    let fixture_root = std::env::var("RUSTERISK_REFERENCE_CONFIGURATION")
        .map_err(|_| Error::Invalid("reference configuration directory is required"))?;
    let (storage, storage_worker) = sounds::prepare(std::path::Path::new(&fixture_root)).await?;
    let (controller, runner, effects) = rusterisk_server::Controller::new(configuration.clone(), 4);
    let (media, hub, notices) = rusterisk_server::media::MediaHub::new(4);
    let signaling = rusterisk_server::signaling::Signaling::bind(
        configuration,
        controller.clone(),
        effects,
        media.clone(),
        notices,
    )
    .await
    .map_err(|_| Error::Invalid("native signaling"))?
    .with_storage(storage.clone());
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let runner = tokio::spawn(
        runner
            .with_storage(storage)
            .map_err(|_| Error::Invalid("reference storage"))?
            .run(),
    );
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(
            listener,
            rusterisk_server::http::router_with_media(controller.clone(), media),
        )
        .into_future(),
    );
    let result = async {
        let actual = Client::new(&format!("http://{address}"), "app", "test-password")?;
        server_info::compare(
            &actual,
            &format!("http://{address}"),
            &reference,
            &reference_address,
        )
        .await?;
        sounds::compare(&actual, &reference).await?;
        assert_eq!(actual.channels().await?, reference.channels().await?);
        assert_eq!(
            actual.applications().await?,
            reference.applications().await?
        );
        compare_bridges(&actual, &reference).await?;
        request_bodies::compare(
            &actual,
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
        compare_external_admission(&actual, &mut actual_session, app.as_str()).await?;
        compare_external_admission(&reference, &mut reference_session, app.as_str()).await?;
        request_bodies::originate_with_id(&actual, &format!("http://{address}"), app.as_str())
            .await?;
        request_bodies::originate_with_id(&reference, &reference_address, app.as_str()).await?;
        request_bodies::initial_variables(&actual, &format!("http://{address}"), app.as_str())
            .await?;
        request_bodies::initial_variables(&reference, &reference_address, app.as_str()).await?;
        request_bodies::websocket_variables(&actual, &format!("http://{address}"), app.as_str())
            .await?;
        request_bodies::websocket_variables(&reference, &reference_address, app.as_str()).await?;
        external_audio::compare(
            &actual,
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
            &actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        playback_position::compare(
            &actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        recording_audio::compare(
            &actual,
            &mut actual_session,
            app.as_str(),
            Ipv4Addr::LOCALHOST,
        )
        .await?;
        external_streams::compare(
            &actual,
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
            &actual,
            &format!("http://{address}"),
            &mut actual_session,
            app.as_str(),
        )
        .await?;
        external_opus::compare(
            &reference,
            &reference_address,
            &mut reference_session,
            app.as_str(),
        )
        .await?;
        external_opus::compare(
            &actual,
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
        external_websocket_clients::compare(&actual, &mut actual_session, app.as_str(), outgoing)
            .await?;
        compare_subscriptions(&actual, &reference, app.as_str()).await?;
        reference_session.shutdown().await?;
        actual_session.shutdown().await?;
        compare_existing_client(&format!("http://{address}"), &reference_address).await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    signaling
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    hub.await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    server.abort();
    assert!(server.await.is_err());
    let cleanup = storage_worker
        .close()
        .await
        .map_err(|error| Error::Io(std::io::Error::other(error.to_string())));
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; reference storage cleanup failed: {cleanup}"
        ))),
    }
}
