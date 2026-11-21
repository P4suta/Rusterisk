use rusterisk_client::{api::StaticRtpCodec, Error};
use rusterisk_media::{alaw_decode, mulaw_decode, RtpPacket};
use rusterisk_server::rtp::SocketPair;
use std::time::Duration;

pub(crate) async fn received(
    peer: &SocketPair,
    codec: StaticRtpCodec,
    expected: i32,
) -> Result<(), Error> {
    let mut packets = 0;
    let mut last = None;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = [0; 2049];
        for _ in 0..150 {
            let (size, source) = peer.data().recv_from(&mut bytes).await?;
            let packet = RtpPacket::parse(&bytes[..size]).map_err(|error| {
                eprintln!("External audio comparison: {error}");
                Error::Invalid("external audio comparison")
            })?;
            packets += 1;
            last = Some((source, packet.payload().len()));
            if packet.payload().len() == 160
                && packet.payload().iter().all(|byte| {
                    let sample = match codec {
                        StaticRtpCodec::Pcmu => mulaw_decode(*byte),
                        StaticRtpCodec::Pcma => alaw_decode(*byte),
                    };
                    (i32::from(sample) - expected).abs() < 300
                })
            {
                return Ok(());
            }
        }
        Err(Error::Invalid("expected received audio"))
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    if result.is_err() {
        eprintln!("external audio receive failed: codec={codec:?}, expected={expected}, packets={packets}, last={last:?}");
    }
    result
}
