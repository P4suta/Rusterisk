use rusterisk_client::{
    api::{AudioSocketTarget, ChannelId},
    Client, Error, Session,
};
use rusterisk_media::{AudioSocket, Frame};
use std::{net::Ipv4Addr, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

fn invalid(error: impl std::fmt::Display) -> Error {
    eprintln!("External stream comparison: {error}");
    Error::Invalid("external stream comparison")
}

async fn received(peer: &mut (impl AsyncRead + Unpin), expected: i16) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = [0; 1923];
        for _ in 0..150 {
            peer.read_exact(&mut bytes[..3]).await?;
            let length = usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
            if length > 1920 { return Err(Error::Capacity); }
            peer.read_exact(&mut bytes[3..length + 3]).await?;
            if matches!(AudioSocket::parse(&bytes[..length + 3]).map_err(invalid)?, AudioSocket::Audio(frame) if frame.samples() == [expected; 160]) { return Ok(()); }
        }
        Err(Error::Invalid("expected received stream audio"))
    }).await.map_err(|error| invalid(format!("Receive {expected}: {error}")))?
}

pub async fn compare(
    client: &Client,
    session: &mut Session,
    app: &str,
    callback_ip: Ipv4Addr,
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let ids = [
        ChannelId::try_from("stream-left".to_owned())?,
        ChannelId::try_from("stream-right".to_owned())?,
    ];
    let mut peers = Vec::new();
    for (id, identity) in [(&ids[0], [7; 16]), (&ids[1], [8; 16])] {
        let target = AudioSocketTarget::new(
            std::net::SocketAddr::new(callback_ip.into(), listener.local_addr()?.port()),
            identity,
        )
        .map_err(invalid)?;
        let channel = client.external_audio_socket(id, app, target).await?;
        assert_eq!(channel.id, *id);
        assert!(channel.name.starts_with("AudioSocket/"));
        let (mut peer, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .map_err(invalid)??;
        let mut header = [0; 19];
        tokio::time::timeout(Duration::from_secs(2), peer.read_exact(&mut header))
            .await
            .map_err(invalid)??;
        assert_eq!(
            AudioSocket::parse(&header).map_err(invalid)?,
            AudioSocket::Uuid(identity)
        );
        super::external_audio::started(session, id).await?;
        peers.push(peer);
    }
    let bridge = ChannelId::try_from("stream-bridge".to_owned())?;
    client.create_bridge(&bridge, "Stream comparison").await?;
    client.add_channels(&bridge, &ids).await?;
    let mut left = peers.remove(0);
    let mut right = peers.remove(0);
    let (mut left_read, mut left_write) = left.split();
    let (mut right_read, mut right_write) = right.split();
    let send = async {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let left = AudioSocket::Audio(Frame::new(&[4000; 160], 8000).map_err(invalid)?)
            .encode()
            .map_err(invalid)?;
        let right = AudioSocket::Audio(Frame::new(&[-7000; 160], 8000).map_err(invalid)?)
            .encode()
            .map_err(invalid)?;
        for _ in 0..30 {
            tick.tick().await;
            left_write.write_all(&left).await?;
            right_write.write_all(&right).await?;
        }
        Ok::<(), Error>(())
    };
    tokio::try_join!(
        send,
        received(&mut left_read, -7000),
        received(&mut right_read, 4000)
    )?;
    client.remove_channels(&bridge, &ids).await?;
    for id in &ids {
        client.hangup(id).await?;
    }
    client.destroy_bridge(&bridge).await?;
    Ok(())
}
