pub(super) use crate::received_audio::received;
use rusterisk_client::{
    api::{Channel, ChannelId, EventKind, ExternalRtpTarget, StaticRtpCodec},
    Client, Error, Session,
};
use rusterisk_media::{AudioFormat, Frame, RtpSender};
use rusterisk_server::rtp::SocketPair;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

fn invalid(error: impl std::fmt::Display) -> Error {
    eprintln!("External audio comparison: {error}");
    Error::Invalid("external audio comparison")
}

pub(super) async fn started(session: &mut Session, id: &ChannelId) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(3), async {
        for _ in 0..64 {
            if let EventKind::StasisStart { channel, .. } = session.next_event().await?.kind {
                if channel.id == *id {
                    return Ok(());
                }
            }
        }
        Err(Error::Capacity)
    })
    .await
    .map_err(|_| Error::Timeout)?
}

fn local(channel: Channel) -> Result<SocketAddr, Error> {
    let variables = channel
        .channelvars
        .ok_or(Error::Invalid("transport variables"))?;
    let ip = variables
        .get("UNICASTRTP_LOCAL_ADDRESS")
        .ok_or(Error::Invalid("transport address"))?
        .parse::<IpAddr>()
        .map_err(invalid)?;
    let port = variables
        .get("UNICASTRTP_LOCAL_PORT")
        .ok_or(Error::Invalid("transport port"))?
        .parse::<u16>()
        .map_err(invalid)?;
    if ip.is_unspecified() || port == 0 {
        return Err(Error::Invalid("transport variables"));
    }
    Ok(SocketAddr::new(ip, port))
}

pub(super) fn local_media_ip(server_ip: Ipv4Addr) -> Result<IpAddr, Error> {
    let route = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    route.connect((server_ip, 9))?;
    Ok(route.local_addr()?.ip())
}

pub async fn compare(
    client: &Client,
    session: &mut Session,
    app: &str,
    server_ip: Ipv4Addr,
) -> Result<(), Error> {
    let host_ip = local_media_ip(server_ip)?;
    for codec in [StaticRtpCodec::Pcmu, StaticRtpCodec::Pcma] {
        let left = SocketPair::bind(host_ip, 0, 0)?;
        let right = SocketPair::bind(host_ip, 0, 0)?;
        let ids = [
            ChannelId::try_from(format!("audio-left-{}", codec.as_str()))?,
            ChannelId::try_from(format!("audio-right-{}", codec.as_str()))?,
        ];
        let mut destinations = Vec::new();
        for (id, peer) in [(&ids[0], &left), (&ids[1], &right)] {
            let target = ExternalRtpTarget::new(peer.local_addr()?, codec).map_err(invalid)?;
            let channel = client.external_rtp(id, app, target, "audio").await?;
            destinations.push(local(channel)?);
            started(session, id).await?;
        }
        let bridge =
            rusterisk_client::BridgeId::try_from(format!("audio-bridge-{}", codec.as_str()))?;
        client.create_bridge(&bridge, "Audio comparison").await?;
        client.add_channels(&bridge, &ids).await?;
        let send = async {
            let format = match codec {
                StaticRtpCodec::Pcmu => AudioFormat::Pcmu,
                StaticRtpCodec::Pcma => AudioFormat::Pcma,
            };
            let mut first =
                RtpSender::new(format, codec.payload_type(), [31; 10]).map_err(invalid)?;
            let mut second =
                RtpSender::new(format, codec.payload_type(), [32; 10]).map_err(invalid)?;
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for _ in 0..150 {
                tick.tick().await;
                for (peer, destination, sender, value) in [
                    (&left, destinations[0], &mut first, 4000),
                    (&right, destinations[1], &mut second, -7000),
                ] {
                    let bytes = sender
                        .packet(Frame::new(&[value; 160], 8000).map_err(invalid)?)
                        .map_err(invalid)?
                        .encode()
                        .map_err(invalid)?;
                    peer.data().send_to(&bytes, destination).await?;
                }
            }
            Ok::<(), Error>(())
        };
        let receive = async {
            tokio::try_join!(received(&left, codec, -7000), received(&right, codec, 4000))?;
            Ok::<(), Error>(())
        };
        tokio::select! {
            result = receive => result?,
            result = send => {
                result?;
                return Err(Error::Timeout);
            }
        }
        client.remove_channels(&bridge, &ids).await?;
        for id in &ids {
            client.hangup(id).await?;
        }
        client.destroy_bridge(&bridge).await?;
    }
    Ok(())
}
