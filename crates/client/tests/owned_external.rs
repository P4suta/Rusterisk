use rusterisk_client::{
    api::{AudioSocketTarget, ChannelId, ExternalRtpTarget, StaticRtpCodec},
    CallMilestone, Client, Error,
};
use rusterisk_media::{mulaw_decode, AudioFormat, AudioSocket, Frame, RtpPacket, RtpSender};
use rusterisk_server::{media::MediaHub, rtp::SocketPair, signaling::Signaling, Controller};
use std::{
    future::IntoFuture,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

fn combine(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; cleanup failed: {cleanup}"
        ))),
    }
}

async fn joined<E: std::fmt::Display>(
    mut task: tokio::task::JoinHandle<Result<(), E>>,
) -> Result<(), Error> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(Error::Timeout),
                Err(error) => Err(failure(format!("{primary}; join failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(())) => Err(Error::Timeout),
            }
        }
    }
}

async fn message(socket: &mut tokio::net::TcpStream) -> Result<AudioSocket, Error> {
    let mut bytes = [0; 1923];
    socket.read_exact(&mut bytes[..3]).await?;
    let length = 3 + usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
    if length > bytes.len() {
        return Err(Error::Capacity);
    }
    socket.read_exact(&mut bytes[3..length]).await?;
    AudioSocket::parse(&bytes[..length]).map_err(failure)
}

#[tokio::test]
async fn owned_rtp_and_audio_socket_calls_exchange_received_audio_and_close_their_transports(
) -> Result<(), Error> {
    let rtp = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0)?;
    let incoming = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media.clone(), notices)
        .await
        .map_err(failure)?;
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let http = tokio::spawn(
        axum::serve(
            listener,
            rusterisk_server::http::router_with_media(controller.clone(), media),
        )
        .into_future(),
    );
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let session = client
            .subscribe(&ChannelId::try_from("owned-external".to_owned())?)
            .await?;
        let operation = async {
            let target =
                ExternalRtpTarget::new(rtp.local_addr()?, StaticRtpCodec::Pcmu).map_err(failure)?;
            let mut left = session.external_rtp(target, "left,known-audio").await?;
            let identity = [0xab; 16];
            let target = AudioSocketTarget::new(incoming.local_addr()?, identity)
                .map_err(failure)?
                .with_session_text("ABABABAB-ABAB-ABAB-ABAB-ABABABABABAB")
                .map_err(failure)?;
            let mut right = session.external_audio_socket(target).await?;
            let (mut stream, _) = incoming.accept().await?;
            assert_eq!(
                message(&mut stream).await.map_err(failure)?,
                AudioSocket::Uuid(identity)
            );
            let mut rtp_address: Option<SocketAddr> = None;
            for call in [&mut left, &mut right] {
                let accepted = call
                    .wait(CallMilestone::Accepted, Duration::from_secs(1))
                    .await?;
                if let Some(variables) = accepted.channel.and_then(|channel| channel.channelvars) {
                    if let (Some(ip), Some(port)) = (
                        variables.get("UNICASTRTP_LOCAL_ADDRESS"),
                        variables.get("UNICASTRTP_LOCAL_PORT"),
                    ) {
                        rtp_address = Some(format!("{ip}:{port}").parse().map_err(failure)?);
                    }
                }
                call.wait(CallMilestone::Stasis, Duration::from_secs(1))
                    .await?;
                call.wait(CallMilestone::Answered, Duration::from_secs(1))
                    .await?;
            }
            let destination = rtp_address.ok_or(Error::Invalid("missing RTP transport"))?;
            let ids = [left.id().clone(), right.id().clone()];
            let bridge = ChannelId::try_from("owned-external-bridge".to_owned())?;
            client
                .create_bridge(&bridge, "Owned external audio")
                .await?;
            client.add_channels(&bridge, &ids).await?;
            let (mut reader, mut writer) = stream.split();
            let send = async {
                let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [31; 10]).map_err(failure)?;
                let audio = AudioSocket::Audio(Frame::new(&[-7000; 160], 8000).map_err(failure)?)
                    .encode()
                    .map_err(failure)?;
                let mut tick = tokio::time::interval(Duration::from_millis(20));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                for _ in 0..100 {
                    tick.tick().await;
                    let bytes = sender
                        .packet(Frame::new(&[4000; 160], 8000).map_err(failure)?)
                        .map_err(failure)?
                        .encode()
                        .map_err(failure)?;
                    rtp.data().send_to(&bytes, destination).await?;
                    writer.write_all(&audio).await?;
                }
                Err::<(), Error>(Error::Timeout)
            };
            let receive = async {
                let from_rtp = async {
                    let mut bytes = [0; 2049];
                    for _ in 0..150 {
                        let (size, source) = rtp.data().recv_from(&mut bytes).await?;
                        if source != destination {
                            return Err(Error::Invalid("unexpected RTP source"));
                        }
                        let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
                        if packet.payload().len() == 160
                            && packet
                                .payload()
                                .iter()
                                .all(|byte| (i32::from(mulaw_decode(*byte)) + 7000).abs() < 300)
                        {
                            return Ok::<(), Error>(());
                        }
                    }
                    Err(Error::Invalid("missing AudioSocket signal"))
                };
                let from_stream = async {
                    let mut bytes = [0; 1923];
                    for _ in 0..150 {
                        reader.read_exact(&mut bytes[..3]).await?;
                        let length = 3 + usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
                        if length > bytes.len() {
                            return Err(Error::Capacity);
                        }
                        reader.read_exact(&mut bytes[3..length]).await?;
                        if let AudioSocket::Audio(frame) =
                            AudioSocket::parse(&bytes[..length]).map_err(failure)?
                        {
                            if frame.samples().len() == 160
                                && frame
                                    .samples()
                                    .iter()
                                    .all(|sample| (i32::from(*sample) - 4000).abs() < 300)
                            {
                                return Ok::<(), Error>(());
                            }
                        }
                    }
                    Err(Error::Invalid("missing RTP signal"))
                };
                tokio::try_join!(from_rtp, from_stream)?;
                Ok::<(), Error>(())
            };
            tokio::select! { result = send => result?, result = receive => result? }
            for call in [&mut left, &mut right] {
                call.wait(CallMilestone::MediaConnected, Duration::from_secs(1))
                    .await?;
            }
            left.close().await?;
            right.close().await?;
            let mut terminated = false;
            for _ in 0..150 {
                if message(&mut stream).await? == AudioSocket::Terminate {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(Error::Invalid("AudioSocket did not terminate"));
            }
            for id in &ids {
                if !matches!(
                    client.channel(id).await,
                    Err(Error::Api { status: 404, .. })
                ) {
                    return Err(Error::Invalid("owned channel survived close"));
                }
            }
            let released = tokio::net::UdpSocket::bind(destination).await?;
            drop(released);
            client.destroy_bridge(&bridge).await?;
            Ok::<(), Error>(())
        }
        .await;
        combine(operation, session.shutdown().await)
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    controller.shutdown();
    let result = combine(result, joined(runner).await);
    let result = combine(result, joined(signaling).await);
    let result = combine(result, joined(hub).await);
    http.abort();
    combine(
        result,
        match http.await {
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(failure(error)),
            Ok(result) => result.map_err(failure),
        },
    )
}
