use rusterisk_client::{
    api::{
        ChannelId, EventKind, EventSource, ExternalRtpTarget, MediaUri, PlaybackMilliseconds,
        PlaybackOptions, PlaybackState, SoundId, StaticRtpCodec,
    },
    Client, Error, Session,
};
use rusterisk_media::{mulaw_decode, RtpPacket};
use rusterisk_server::rtp::SocketPair;
use std::{
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};

const SEEK_START_MS: u32 = 22_000;
const SEEK_STEP_MS: u32 = 7000;
pub(super) const INTERRUPTION_OFFSET_MS: u32 = 24_000;

pub fn prepare(root: &Path) -> Result<(), Error> {
    let directory = root.join("sounds/en");
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("rusterisk-position.wav"))?;
    write_position_sound(file)
}

fn write_position_sound(mut file: impl Write) -> Result<(), Error> {
    let segments = [
        (3_u32, 4000_i16),
        (3, -7000),
        (1, 1000),
        (1, 2000),
        (6, -7000),
        (4, 1000),
        (3, 4000),
        (4, 2000),
    ];
    let data_bytes: u32 = segments.iter().map(|(seconds, _)| seconds * 16_000).sum();
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&8000_u32.to_le_bytes())?;
    file.write_all(&16_000_u32.to_le_bytes())?;
    file.write_all(&2_u16.to_le_bytes())?;
    file.write_all(&16_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    for (seconds, sample) in segments {
        let mut second = [0; 16_000];
        for bytes in second.as_chunks_mut::<2>().0 {
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
        for _ in 0..seconds {
            file.write_all(&second)?;
        }
    }
    Ok(())
}

struct AudioWindow {
    expected: i32,
    last: Option<(u32, u16, u32)>,
    consecutive: u8,
}

impl AudioWindow {
    fn new(expected: i32) -> Self {
        Self {
            expected,
            last: None,
            consecutive: 0,
        }
    }

    fn observe(&mut self, packet: &RtpPacket) -> bool {
        if packet.payload_type() != 0
            || packet.payload().len() != 160
            || !packet
                .payload()
                .iter()
                .all(|byte| (i32::from(mulaw_decode(*byte)) - self.expected).abs() < 300)
        {
            self.last = None;
            self.consecutive = 0;
            return false;
        }
        self.consecutive = if self.last.is_some_and(|(ssrc, sequence, timestamp)| {
            ssrc == packet.ssrc()
                && sequence.wrapping_add(1) == packet.sequence()
                && timestamp.wrapping_add(160) == packet.timestamp()
        }) {
            self.consecutive.saturating_add(1).min(4)
        } else {
            1
        };
        self.last = Some((packet.ssrc(), packet.sequence(), packet.timestamp()));
        self.consecutive == 4
    }
}

async fn received(peer: &SocketPair, sample: i32, phase: &'static str) -> Result<(), Error> {
    received_by(
        peer,
        sample,
        phase,
        tokio::time::Instant::now() + Duration::from_secs(2),
    )
    .await
}

async fn received_by(
    peer: &SocketPair,
    sample: i32,
    phase: &'static str,
    deadline: tokio::time::Instant,
) -> Result<(), Error> {
    let mut packets = 0;
    let mut last = None;
    let result = tokio::time::timeout_at(deadline, async {
        let mut window = AudioWindow::new(sample);
        let mut bytes = [0; 2049];
        for _ in 0..64 {
            let (size, _) = peer.data().recv_from(&mut bytes).await?;
            let packet = RtpPacket::parse(&bytes[..size])
                .map_err(|_| Error::Invalid("playback audio packet"))?;
            packets += 1;
            last = Some((
                packet.payload_type(),
                packet.payload().len(),
                packet.sequence(),
                packet.timestamp(),
                packet.ssrc(),
                packet.payload().first().copied().map(mulaw_decode),
            ));
            if window.observe(&packet) {
                return Ok(());
            }
        }
        Err(Error::Capacity)
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    if let Err(error) = &result {
        eprintln!("playback audio failed: phase={phase}, expected={sample}, packets={packets}, last={last:?}, error={error}");
    }
    result
}

#[derive(Clone, Copy, Debug)]
enum Observation<'a> {
    Stasis(&'a ChannelId),
    Finished(&'a ChannelId),
}

async fn observed(session: &mut Session, wanted: Observation<'_>) -> Result<(), Error> {
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        for _ in 0..64 {
            let kind = session.next_event().await?.kind;
            match (&wanted, kind) {
                (Observation::Stasis(id), EventKind::StasisStart { channel, .. })
                    if channel.id == **id =>
                {
                    return Ok(())
                }
                (Observation::Finished(id), EventKind::PlaybackFinished { playback: ended })
                    if ended.id == **id =>
                {
                    return if ended.state == PlaybackState::Done {
                        Ok(())
                    } else {
                        Err(Error::Invalid("playback finished unsuccessfully"))
                    };
                }
                _ => {}
            }
        }
        Err(Error::Capacity)
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    if let Err(error) = &result {
        eprintln!("playback event wait failed: wanted={wanted:?}, error={error}");
    }
    result
}

fn combine(operation: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{operation}; playback cleanup failed: {cleanup}"
        ))),
    }
}

fn conflict(result: Result<(), Error>) -> Result<(), Error> {
    match result {
        Err(Error::Api { status: 409, .. }) => Ok(()),
        Err(error) => Err(error),
        Ok(()) => Err(Error::Invalid(
            "paused playback accepted a position control",
        )),
    }
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
    let media = MediaUri::Sound(
        SoundId::try_from("rusterisk-position".to_owned())
            .map_err(|_| Error::Invalid("playback fixture name"))?,
    );
    for target in [Target::Channel, Target::Bridge] {
        let label = match target {
            Target::Channel => "channel",
            Target::Bridge => "bridge",
        };
        let id = ChannelId::try_from(format!("position-{label}"))?;
        let bridge = ChannelId::try_from(format!("position-mix-{label}"))?;
        let playback = ChannelId::try_from(format!("position-play-{label}"))?;
        let peer = SocketPair::bind(Ipv4Addr::UNSPECIFIED.into(), 0, 0)?;
        let destination = SocketAddr::new(callback.into(), peer.local_addr()?.port());
        let destination = ExternalRtpTarget::new(destination, StaticRtpCodec::Pcmu)
            .map_err(|_| Error::Invalid("playback peer"))?;
        client
            .external_rtp(&id, application, destination, "playback")
            .await?;
        let operation = async {
            observed(session, Observation::Stasis(&id)).await?;
            let started = tokio::time::Instant::now();
            let accepted = match target {
                Target::Channel => client.play_channel(&id, &playback, &media, None).await?,
                Target::Bridge => {
                    client.create_bridge(&bridge, "Playback position").await?;
                    client
                        .add_channels(&bridge, std::slice::from_ref(&id))
                        .await?;
                    client
                        .subscribe_resources(application, &[EventSource::Bridge(bridge.clone())])
                        .await?;
                    client.play_bridge(&bridge, &playback, &media, None).await?
                }
            };
            if accepted.id != playback || accepted.media_uri != media.to_string() {
                return Err(Error::Invalid("playback admission response"));
            }
            received(&peer, 4000, "start").await?;
            client.forward_playback(&playback).await?;
            received_by(&peer, -7000, "forward", started + Duration::from_secs(2)).await?;
            client.reverse_playback(&playback).await?;
            received(&peer, 4000, "reverse").await?;
            client.forward_playback(&playback).await?;
            received(&peer, -7000, "forward again").await?;
            client.restart_playback(&playback).await?;
            received(&peer, 4000, "restart").await?;
            client.pause_playback(&playback).await?;
            if client.playback(&playback).await?.state != PlaybackState::Paused {
                return Err(Error::Invalid("playback did not enter paused state"));
            }
            conflict(client.restart_playback(&playback).await)?;
            conflict(client.forward_playback(&playback).await)?;
            conflict(client.reverse_playback(&playback).await)?;
            client.resume_playback(&playback).await?;
            received(&peer, 4000, "resume").await?;
            client.stop_playback(&playback).await?;
            observed(session, Observation::Finished(&playback)).await?;
            match client.playback(&playback).await {
                Err(Error::Api { status: 404, .. }) => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(Error::Invalid("finished playback is still live")),
            }
            let options = PlaybackOptions {
                offset_ms: PlaybackMilliseconds::try_from(SEEK_START_MS)
                    .map_err(|_| Error::Invalid("playback fixture offset"))?,
                skip_ms: PlaybackMilliseconds::try_from(SEEK_STEP_MS)
                    .map_err(|_| Error::Invalid("playback fixture step"))?,
            };
            let started = tokio::time::Instant::now();
            match target {
                Target::Channel => {
                    client
                        .play_channel_with_options(&id, &playback, &media, None, options)
                        .await?;
                }
                Target::Bridge => {
                    client
                        .play_bridge_with_options(&bridge, &playback, &media, None, options)
                        .await?;
                }
            }
            received_by(&peer, 2000, "offset", started + Duration::from_secs(1)).await?;
            client.reverse_playback(&playback).await?;
            received(&peer, 1000, "custom reverse").await?;
            client.reverse_playback(&playback).await?;
            received(&peer, -7000, "custom reverse again").await?;
            client.forward_playback(&playback).await?;
            received(&peer, 1000, "custom forward").await?;
            client.forward_playback(&playback).await?;
            received(&peer, 2000, "custom forward again").await?;
            client.restart_playback(&playback).await?;
            received(&peer, 4000, "custom restart").await?;
            client.stop_playback(&playback).await?;
            observed(session, Observation::Finished(&playback)).await
        }
        .await;
        let cleanup = match client.stop_playback(&playback).await {
            Err(Error::Api { status: 404, .. }) => Ok(()),
            result => result,
        };
        let operation = combine(operation, cleanup);
        let operation = match target {
            Target::Channel => operation,
            Target::Bridge => {
                let operation = combine(
                    operation,
                    client
                        .unsubscribe_resources(application, &[EventSource::Bridge(bridge.clone())])
                        .await
                        .map(|_| ()),
                );
                combine(operation, client.destroy_bridge(&bridge).await)
            }
        };
        combine(operation, client.hangup(&id).await)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusterisk_client::Error;
    use rusterisk_media::{mulaw_encode, RtpPacket};

    #[test]
    fn custom_seeks_have_distinct_audio_through_the_receive_deadline() -> Result<(), Error> {
        let mut wave = Vec::new();
        super::write_position_sound(&mut wave)?;
        let sample = |millis: u32| {
            let byte = 44 + millis as usize * 16;
            wave.get(byte..byte + 2)
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        };
        let start = super::SEEK_START_MS;
        let step = super::SEEK_STEP_MS;
        for elapsed in (0..=2000).step_by(20) {
            for (position, expected) in [
                (start, 2000),
                (start - step, 1000),
                (start - 2 * step, -7000),
            ] {
                assert_eq!(
                    sample(position + elapsed),
                    Some(expected),
                    "position {position} + {elapsed}"
                );
            }
            assert_ne!(sample(start - 3000 + elapsed), Some(1000));
        }
        assert_eq!(sample(super::INTERRUPTION_OFFSET_MS), Some(2000));
        assert_eq!(sample(super::INTERRUPTION_OFFSET_MS + 999), Some(2000));
        assert_eq!(sample(super::INTERRUPTION_OFFSET_MS + 1000), None);
        Ok(())
    }

    fn packet(sequence: u16, sample: i16) -> Result<RtpPacket, Error> {
        RtpPacket::new(
            0,
            sequence,
            u32::from(sequence) * 160,
            1234,
            &[mulaw_encode(sample); 160],
        )
        .map_err(|_| Error::Invalid("audio observation fixture"))
    }

    #[test]
    fn playback_audio_requires_eighty_milliseconds_of_consecutive_known_samples(
    ) -> Result<(), Error> {
        let mut window = super::AudioWindow::new(4000);
        for sequence in 0..3 {
            assert!(!window.observe(&packet(sequence, 4000)?));
        }
        assert!(window.observe(&packet(3, 4000)?));
        Ok(())
    }

    #[test]
    fn stale_audio_gaps_duplicates_and_wrong_payloads_reset_the_observation() -> Result<(), Error> {
        let mut window = super::AudioWindow::new(4000);
        for sequence in 0..3 {
            assert!(!window.observe(&packet(sequence, -7000)?));
        }
        assert!(!window.observe(&packet(3, 4000)?));
        assert!(!window.observe(&packet(3, 4000)?));
        assert!(!window.observe(&packet(5, 4000)?));
        assert!(!window.observe(&packet(6, 0)?));
        let wrong = RtpPacket::new(8, 7, 7 * 160, 1234, &[mulaw_encode(4000); 160])
            .map_err(|_| Error::Invalid("audio observation fixture"))?;
        assert!(!window.observe(&wrong));
        for sequence in 8..11 {
            assert!(!window.observe(&packet(sequence, 4000)?));
        }
        assert!(window.observe(&packet(11, 4000)?));
        Ok(())
    }

    #[test]
    fn playback_audio_requires_one_source_clock_and_complete_frames() -> Result<(), Error> {
        for (timestamp, source, length) in [(481, 1234, 160), (480, 1235, 160), (480, 1234, 159)] {
            let mut window = super::AudioWindow::new(4000);
            for sequence in 0..3 {
                assert!(!window.observe(&packet(sequence, 4000)?));
            }
            let changed = RtpPacket::new(
                0,
                3,
                timestamp,
                source,
                &[mulaw_encode(4000); 160][..length],
            )
            .map_err(|_| Error::Invalid("audio observation fixture"))?;
            assert!(!window.observe(&changed));
            for sequence in 4..7 {
                assert!(!window.observe(&packet(sequence, 4000)?));
            }
            assert!(window.observe(&packet(7, 4000)?));
        }
        Ok(())
    }
}
