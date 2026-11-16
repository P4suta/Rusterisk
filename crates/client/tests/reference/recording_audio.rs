use rusterisk_client::{
    api::{
        Channel, ChannelId, EventKind, EventSource, ExternalRtpTarget, RecordingState,
        StaticRtpCodec,
    },
    Client, Error, Session,
};
use rusterisk_media::{AudioFormat, Frame, RtpSender};
use rusterisk_server::rtp::SocketPair;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

fn destination(channel: Channel) -> Result<SocketAddr, Error> {
    let variables = channel
        .channelvars
        .ok_or(Error::Invalid("recording transport variables"))?;
    let address = variables
        .get("UNICASTRTP_LOCAL_ADDRESS")
        .ok_or(Error::Invalid("recording transport address"))?
        .parse::<IpAddr>()
        .map_err(invalid)?;
    let port = variables
        .get("UNICASTRTP_LOCAL_PORT")
        .ok_or(Error::Invalid("recording transport port"))?
        .parse::<u16>()
        .map_err(invalid)?;
    if address.is_unspecified() || port == 0 {
        return Err(Error::Invalid("recording transport destination"));
    }
    Ok(SocketAddr::new(address, port))
}

enum Observation<'a> {
    Stasis(&'a ChannelId),
    RecordingStarted(&'a ChannelId),
    RecordingFinished(&'a ChannelId),
}

async fn observed(session: &mut Session, wanted: Observation<'_>) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(3), async {
        for _ in 0..128 {
            match (&wanted, session.next_event().await?.kind) {
                (Observation::Stasis(id), EventKind::StasisStart { channel, .. })
                    if channel.id == **id =>
                {
                    return Ok(())
                }
                (Observation::RecordingStarted(id), EventKind::RecordingStarted { recording })
                    if recording.name == id.as_str() =>
                {
                    return Ok(())
                }
                (
                    Observation::RecordingFinished(id),
                    EventKind::RecordingFinished { recording },
                ) if recording.name == id.as_str() => {
                    return if recording.state == RecordingState::Done {
                        Ok(())
                    } else {
                        Err(Error::Invalid("recording publication failed"))
                    };
                }
                _ => {}
            }
        }
        Err(Error::Capacity)
    })
    .await
    .map_err(|_| Error::Timeout)?
}

#[derive(Clone, Copy)]
enum Target {
    Channel,
    Bridge,
}

pub async fn compare(
    client: &Client,
    session: &mut Session,
    application: &str,
    callback: Ipv4Addr,
) -> Result<(), Error> {
    for target in [Target::Channel, Target::Bridge] {
        let label = match target {
            Target::Channel => "channel",
            Target::Bridge => "bridge",
        };
        let count = match target {
            Target::Channel => 1,
            Target::Bridge => 2,
        };
        let recording = ChannelId::try_from(format!("recorded-{label}"))?;
        let mut peers = Vec::with_capacity(2);
        let mut ids = Vec::with_capacity(2);
        for index in 0..count {
            let id = ChannelId::try_from(format!("recording-{label}-{index}"))?;
            let peer = SocketPair::bind(Ipv4Addr::UNSPECIFIED.into(), 0, 0)?;
            let remote = SocketAddr::new(callback.into(), peer.local_addr()?.port());
            let target = ExternalRtpTarget::new(remote, StaticRtpCodec::Pcmu).map_err(invalid)?;
            let channel = client
                .external_rtp(&id, application, target, "recording")
                .await?;
            let destination = destination(channel)?;
            observed(session, Observation::Stasis(&id)).await?;
            peers.push((
                peer,
                destination,
                RtpSender::new(AudioFormat::Pcmu, 0, [80 + index as u8; 10]).map_err(invalid)?,
            ));
            ids.push(id);
        }
        let accepted = match target {
            Target::Channel => {
                client
                    .record_channel(&ids[0], &recording, "wav", Default::default())
                    .await?
            }
            Target::Bridge => {
                client.create_bridge(&recording, "Recording audio").await?;
                client.add_channels(&recording, &ids).await?;
                client
                    .subscribe_resources(application, &[EventSource::Bridge(recording.clone())])
                    .await?;
                client
                    .record_bridge(&recording, &recording, "wav", Default::default())
                    .await?
            }
        };
        let uri = match target {
            Target::Channel => format!("channel:{}", ids[0]),
            Target::Bridge => format!("bridge:{recording}"),
        };
        if accepted.name != recording.as_str()
            || accepted.format != "wav"
            || accepted.target_uri != uri
            || !matches!(
                accepted.state,
                RecordingState::Queued | RecordingState::Recording
            )
        {
            return Err(Error::Invalid("recording admission response"));
        }
        observed(session, Observation::RecordingStarted(&recording)).await?;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for _ in 0..20 {
            tick.tick().await;
            for (index, (peer, destination, sender)) in peers.iter_mut().enumerate() {
                let sample = if index == 0 { 4000 } else { -7000 };
                let bytes = sender
                    .packet(Frame::new(&[sample; 160], 8000).map_err(invalid)?)
                    .map_err(invalid)?
                    .encode()
                    .map_err(invalid)?;
                peer.data().send_to(&bytes, *destination).await?;
            }
        }
        client.stop_recording(&recording).await?;
        observed(session, Observation::RecordingFinished(&recording)).await?;
        let mut audio = Vec::with_capacity(16 * 1024);
        client
            .download_stored_recording(&recording, &mut audio)
            .await?;
        let expected = match target {
            Target::Channel => 4000,
            Target::Bridge => -3000,
        };
        if !super::wave::contains(&audio, expected)? {
            return Err(Error::Invalid(
                "stored recording lacks sustained received audio",
            ));
        }
        client.delete_stored_recording(&recording).await?;
        for id in &ids {
            client.hangup(id).await?;
        }
        if matches!(target, Target::Bridge) {
            client
                .unsubscribe_resources(application, &[EventSource::Bridge(recording.clone())])
                .await?;
            client.destroy_bridge(&recording).await?;
        }
    }
    Ok(())
}
