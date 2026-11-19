use rusterisk_client::{
    api::{
        Channel, ChannelId, EventKind, ExternalRtpTarget, MediaUri, PlaybackMilliseconds,
        PlaybackOptions, PlaybackState, SoundId, StaticRtpCodec,
    },
    Client, Error, MuteDirection, Session,
};
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_server::rtp::SocketPair;
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

fn invalid(reason: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(reason.to_string()))
}

fn require(condition: bool, reason: &'static str) -> Result<(), Error> {
    condition.then_some(()).ok_or(Error::Invalid(reason))
}

fn combine(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; MOH comparison cleanup failed: {cleanup}"
        ))),
    }
}

async fn bounded<T>(future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .map_err(|_| Error::Timeout)?
}

fn missing<T>(result: Result<T, Error>) -> Result<(), Error> {
    match result {
        Err(Error::Api { status: 404, .. }) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(Error::Invalid("A retired MOH resource remains accessible")),
    }
}

#[derive(Clone, Copy)]
enum Lifetime {
    Unused,
    Requested,
    Admitted,
    Released,
}

fn deleted(result: Result<(), Error>, lifetime: Lifetime) -> Result<(), Error> {
    match (result, lifetime) {
        (Err(Error::Api { status: 404, .. }), Lifetime::Requested) => Ok(()),
        (
            result,
            Lifetime::Unused | Lifetime::Requested | Lifetime::Admitted | Lifetime::Released,
        ) => result,
    }
}

struct Peer {
    id: ChannelId,
    socket: SocketPair,
    destination: Option<SocketAddr>,
    lifetime: Lifetime,
}

impl Peer {
    fn new(id: &str, local_ip: IpAddr) -> Result<Self, Error> {
        Ok(Self {
            id: ChannelId::try_from(id.to_owned())?,
            socket: SocketPair::bind(local_ip, 0, 0)?,
            destination: None,
            lifetime: Lifetime::Unused,
        })
    }

    fn destination(&self) -> Result<SocketAddr, Error> {
        self.destination
            .ok_or(Error::Invalid("MOH RTP route is absent"))
    }

    fn check_source(&self, source: SocketAddr) -> Result<(), Error> {
        let expected = self.destination()?;
        if source == expected {
            Ok(())
        } else {
            Err(invalid(format_args!(
                "MOH RTP route differs for {}: expected {expected}, received {source}",
                self.id.as_str()
            )))
        }
    }

    fn route(channel: Channel) -> Result<SocketAddr, Error> {
        let variables = channel
            .channelvars
            .ok_or(Error::Invalid("MOH transport variables"))?;
        let ip = variables
            .get("UNICASTRTP_LOCAL_ADDRESS")
            .ok_or(Error::Invalid("MOH transport address"))?
            .parse::<IpAddr>()
            .map_err(invalid)?;
        let port = variables
            .get("UNICASTRTP_LOCAL_PORT")
            .ok_or(Error::Invalid("MOH transport port"))?
            .parse::<u16>()
            .map_err(invalid)?;
        ExternalRtpTarget::new(SocketAddr::new(ip, port), StaticRtpCodec::Pcmu)
            .map(ExternalRtpTarget::remote)
            .map_err(invalid)
    }

    async fn admit(
        &mut self,
        client: &Client,
        session: &mut Session,
        app: &str,
    ) -> Result<(), Error> {
        missing(bounded(client.channel(&self.id)).await)?;
        let target = ExternalRtpTarget::new(self.socket.local_addr()?, StaticRtpCodec::Pcmu)
            .map_err(invalid)?;
        self.lifetime = Lifetime::Requested;
        let channel = bounded(client.external_rtp(&self.id, app, target, "music")).await?;
        self.lifetime = Lifetime::Admitted;
        require(
            channel.id == self.id,
            "MOH external channel identity changed",
        )?;
        self.destination = Some(Self::route(channel)?);
        super::external_audio::started(session, &self.id).await
    }

    fn drain(&self) -> Result<(), Error> {
        let mut bytes = [0; 2049];
        for _ in 0..256 {
            match self.socket.data().try_recv_from(&mut bytes) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Capacity)
    }
}

#[derive(Clone, Copy)]
enum Wanted<'a> {
    Ended(&'a ChannelId),
    Finished(&'a ChannelId),
}

async fn observed(session: &mut Session, wanted: Wanted<'_>) -> Result<(), Error> {
    bounded(async {
        for _ in 0..64 {
            let event = session.next_event().await?.kind;
            match wanted {
                Wanted::Ended(id) => {
                    if let EventKind::StasisEnd { channel } = event {
                        if channel.id == *id {
                            return Ok(());
                        }
                    }
                }
                Wanted::Finished(id) => {
                    if let EventKind::PlaybackFinished { playback } = event {
                        if playback.id == *id {
                            return require(
                                playback.state == PlaybackState::Done,
                                "MOH interrupting playback did not finish naturally",
                            );
                        }
                    }
                }
            }
        }
        Err(Error::Capacity)
    })
    .await
}

struct Resources {
    peers: [Peer; 2],
    bridge: ChannelId,
    bridge_lifetime: Lifetime,
    playback: ChannelId,
    playback_lifetime: Lifetime,
}

impl Resources {
    fn new(local_ip: IpAddr) -> Result<Self, Error> {
        Ok(Self {
            peers: [
                Peer::new("music-output", local_ip)?,
                Peer::new("music-clock", local_ip)?,
            ],
            bridge: ChannelId::try_from("music-bridge".to_owned())?,
            bridge_lifetime: Lifetime::Unused,
            playback: ChannelId::try_from("music-interruption".to_owned())?,
            playback_lifetime: Lifetime::Unused,
        })
    }

    async fn prepare(
        &mut self,
        client: &Client,
        session: &mut Session,
        app: &str,
    ) -> Result<(), Error> {
        missing(bounded(client.bridge(&self.bridge)).await)?;
        missing(bounded(client.playback(&self.playback)).await)?;
        for peer in &mut self.peers {
            peer.admit(client, session, app).await?;
        }
        self.bridge_lifetime = Lifetime::Requested;
        let bridge = bounded(client.create_bridge(&self.bridge, "Music comparison")).await?;
        self.bridge_lifetime = Lifetime::Admitted;
        require(bridge.id == self.bridge, "MOH bridge identity changed")?;
        bounded(client.add_channels(
            &self.bridge,
            &[self.peers[0].id.clone(), self.peers[1].id.clone()],
        ))
        .await
    }

    async fn cleanup(&mut self, client: &Client, session: &mut Session) -> Result<(), Error> {
        let mut cleanup = Ok(());
        if matches!(
            self.playback_lifetime,
            Lifetime::Requested | Lifetime::Admitted
        ) {
            cleanup = combine(
                cleanup,
                deleted(
                    bounded(client.stop_playback(&self.playback)).await,
                    self.playback_lifetime,
                ),
            );
            cleanup = combine(
                cleanup,
                missing(bounded(client.playback(&self.playback)).await),
            );
        }
        for peer in &mut self.peers {
            match peer.lifetime {
                Lifetime::Unused | Lifetime::Released => {}
                Lifetime::Requested | Lifetime::Admitted => {
                    cleanup = combine(
                        cleanup,
                        deleted(bounded(client.hangup(&peer.id)).await, peer.lifetime),
                    );
                    if matches!(peer.lifetime, Lifetime::Admitted) {
                        cleanup =
                            combine(cleanup, observed(session, Wanted::Ended(&peer.id)).await);
                    }
                    cleanup = combine(cleanup, missing(bounded(client.channel(&peer.id)).await));
                    peer.lifetime = Lifetime::Released;
                }
            }
        }
        if matches!(
            self.bridge_lifetime,
            Lifetime::Requested | Lifetime::Admitted
        ) {
            cleanup = combine(
                cleanup,
                deleted(
                    bounded(client.destroy_bridge(&self.bridge)).await,
                    self.bridge_lifetime,
                ),
            );
            cleanup = combine(cleanup, missing(bounded(client.bridge(&self.bridge)).await));
            self.bridge_lifetime = Lifetime::Released;
        }
        cleanup
    }
}

async fn pump(peers: &[Peer; 2], mut stopped: watch::Receiver<bool>) -> Result<(), Error> {
    let mut senders = [
        RtpSender::new(AudioFormat::Pcmu, 0, [91; 10]).map_err(invalid)?,
        RtpSender::new(AudioFormat::Pcmu, 0, [92; 10]).map_err(invalid)?,
    ];
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stopped.borrow() {
            return Ok(());
        }
        tokio::select! {
            biased;
            changed = stopped.changed() => { changed.map_err(|_| Error::Closed)?; }
            _instant = tick.tick() => {
                for (peer, sender) in peers.iter().zip(&mut senders) {
                    let bytes = sender.packet(Frame::new(&[0; 160], 8000).map_err(invalid)?)
                        .map_err(invalid)?.encode().map_err(invalid)?;
                    let sent = tokio::time::timeout(Duration::from_secs(1), peer.socket.data().send_to(&bytes, peer.destination()?))
                        .await.map_err(|_| Error::Timeout)??;
                    require(sent == bytes.len(), "MOH clock sent a partial RTP datagram")?;
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub enum RtpClock {
    FirstSample,
    Asterisk,
}

struct AudioWindow {
    expected: i32,
    clock: RtpClock,
    last: Option<(u32, u16, u32, u32)>,
    first: Option<Instant>,
    samples: u32,
    total_samples: u32,
}

impl AudioWindow {
    fn new(expected: i32, clock: RtpClock) -> Self {
        Self {
            expected,
            clock,
            last: None,
            first: None,
            samples: 0,
            total_samples: 0,
        }
    }

    fn observe(&mut self, packet: &RtpPacket, now: Instant) -> Result<(), Error> {
        let samples = u32::try_from(packet.payload().len()).map_err(invalid)?;
        if packet.payload_type() != 0 || !(1..=1600).contains(&samples) {
            return Err(invalid(format_args!(
                "MOH audio has payload type {}, {samples} samples, sequence {}, timestamp {}, previous {:?}",
                packet.payload_type(),
                packet.sequence(),
                packet.timestamp(),
                self.last
            )));
        }
        let known = packet
            .payload()
            .iter()
            .all(|byte| (i32::from(mulaw_decode(*byte)) - self.expected).abs() < 300);
        if !known {
            return require(
                self.first.is_none(),
                "MOH audio changed after the measurement began",
            );
        }
        let mut restart = false;
        if let Some((ssrc, sequence, timestamp, previous_samples)) = self.last {
            let expected_sequence = sequence.wrapping_add(1);
            let step = match self.clock {
                RtpClock::FirstSample => previous_samples,
                RtpClock::Asterisk => samples,
            };
            let consecutive = ssrc == packet.ssrc() && expected_sequence == packet.sequence();
            let elapsed_samples = packet.timestamp().wrapping_sub(timestamp);
            let continuous = consecutive && elapsed_samples == step;
            restart = consecutive
                && elapsed_samples < (1_u32 << 31)
                && elapsed_samples
                    .checked_sub(step)
                    .is_some_and(|gap| gap != 0 && gap.is_multiple_of(160));
            if !continuous && !restart {
                eprintln!(
                    "MOH RTP clock mismatch: previous=(ssrc={ssrc}, sequence={sequence}, timestamp={timestamp}, samples={previous_samples}), expected=(ssrc={ssrc}, sequence={expected_sequence}, timestamp={}), observed=(ssrc={}, sequence={}, timestamp={}, samples={samples})",
                    timestamp.wrapping_add(step),
                    packet.ssrc(),
                    packet.sequence(),
                    packet.timestamp(),
                );
            }
            require(
                continuous || restart,
                "MOH audio lost its continuous RTP clock",
            )?;
        }
        let total_samples = self
            .total_samples
            .checked_add(samples)
            .ok_or(Error::Capacity)?;
        self.samples = if restart {
            self.first = Some(now);
            samples
        } else {
            self.first.get_or_insert(now);
            self.samples.checked_add(samples).ok_or(Error::Capacity)?
        };
        self.last = Some((
            packet.ssrc(),
            packet.sequence(),
            packet.timestamp(),
            samples,
        ));
        self.total_samples = total_samples;
        Ok(())
    }

    fn complete(&self, frames: u16, now: Instant) -> bool {
        self.samples >= u32::from(frames) * 160
            && self.first.is_some_and(|first| {
                now.duration_since(first)
                    >= Duration::from_millis(u64::from(frames.saturating_sub(1)) * 20)
            })
    }
}

fn repeat_complete(
    observed: u32,
    repeated: bool,
    post_repeat: Option<(u32, Instant)>,
    now: Instant,
) -> bool {
    repeated
        && post_repeat.is_none_or(|(count, at)| {
            observed >= count + 12 * 160 && now.duration_since(at) >= Duration::from_millis(220)
        })
}

async fn audio(
    client: &Client,
    peers: &[Peer; 2],
    clock: RtpClock,
    sample: i32,
    frames: u16,
    repeat: Option<&str>,
    phase: &'static str,
) -> Result<(), Error> {
    let [left, right] = peers;
    left.drain()?;
    right.drain()?;
    let mut window = AudioWindow::new(sample, clock);
    let mut repeated = repeat.is_none();
    let mut post_repeat = None;
    let request = async {
        match repeat {
            Some(class) => bounded(client.start_music_on_hold(&left.id, Some(class))).await,
            None => Ok(()),
        }
    };
    tokio::pin!(request);
    let mut first = [0; 2049];
    let mut second = [0; 2049];
    let result = bounded(async {
        for _ in 0..256 {
            tokio::select! {
                result = &mut request, if !repeated && window.total_samples >= 20 * 160 => {
                    result?;
                    repeated = true;
                    post_repeat = Some((window.total_samples, Instant::now()));
                }
                result = left.socket.data().recv_from(&mut first) => {
                    let (size, source) = result?;
                    left.check_source(source)?;
                    let packet = RtpPacket::parse(&first[..size]).map_err(invalid)?;
                    window.observe(&packet, Instant::now())?;
                }
                result = right.socket.data().recv_from(&mut second) => {
                    let (_, source) = result?;
                    right.check_source(source)?;
                }
            }
            let now = Instant::now();
            if window.complete(frames, now)
                && repeat_complete(window.total_samples, repeated, post_repeat, now)
            {
                return Ok(());
            }
        }
        Err(Error::Capacity)
    })
    .await;
    if let Err(error) = &result {
        eprintln!(
            "MOH audio comparison failed: phase={phase}, samples={}, total_samples={}, error={error}",
            window.samples, window.total_samples
        );
    }
    result
}

async fn exercise(
    client: &Client,
    session: &mut Session,
    peers: &[Peer; 2],
    clock: RtpClock,
    playback_id: &ChannelId,
    playback_lifetime: &mut Lifetime,
) -> Result<(), Error> {
    let id = peers[0].id.clone();
    let receive =
        |sample, frames, repeat, phase| audio(client, peers, clock, sample, frames, repeat, phase);
    super::external_audio::received(&peers[0].socket, StaticRtpCodec::Pcmu, 0).await?;
    receive(0, 12, None, "initial received silence").await?;
    bounded(client.start_music_on_hold(&id, None)).await?;
    receive(1200, 60, None, "default one-second loop").await?;
    bounded(client.stop_music_on_hold(&id)).await?;
    receive(0, 12, None, "stopped default").await?;
    bounded(client.start_music_on_hold(&id, Some("Main"))).await?;
    receive(2400, 60, Some("Main"), "Main loop and idempotent start").await?;
    bounded(client.stop_music_on_hold(&id)).await?;
    receive(0, 12, None, "stopped Main").await?;
    bounded(client.start_music_on_hold(&id, Some("mAiN"))).await?;
    receive(2400, 12, None, "case-insensitive fresh Main").await?;
    bounded(client.stop_music_on_hold(&id)).await?;
    receive(0, 12, None, "stopped mixed-case Main").await?;
    bounded(client.start_music_on_hold(&id, Some("missing-music-class"))).await?;
    receive(1200, 12, None, "unknown class fallback").await?;
    bounded(client.mute(&id, MuteDirection::Outgoing)).await?;
    receive(0, 12, None, "muted MOH").await?;
    bounded(client.unmute(&id, MuteDirection::Outgoing)).await?;
    receive(1200, 12, None, "unmuted MOH").await?;
    let media =
        MediaUri::Sound(SoundId::try_from("rusterisk-position".to_owned()).map_err(invalid)?);
    let options = PlaybackOptions {
        offset_ms: PlaybackMilliseconds::try_from(7000).map_err(invalid)?,
        skip_ms: PlaybackMilliseconds::try_from(1000).map_err(invalid)?,
    };
    *playback_lifetime = Lifetime::Requested;
    let playback =
        bounded(client.play_channel_with_options(&id, playback_id, &media, None, options)).await?;
    *playback_lifetime = Lifetime::Admitted;
    require(
        playback.id == *playback_id && playback.media_uri == media.to_string(),
        "MOH interrupting playback identity changed",
    )?;
    receive(2000, 12, None, "ordinary playback interrupts music").await?;
    observed(session, Wanted::Finished(playback_id)).await?;
    missing(bounded(client.playback(playback_id)).await)?;
    *playback_lifetime = Lifetime::Released;
    receive(
        0,
        12,
        None,
        "natural playback completion does not resume music",
    )
    .await?;
    bounded(client.start_music_on_hold(&id, Some("Main"))).await?;
    receive(2400, 12, None, "active music before terminal cleanup").await
}

pub async fn compare(
    client: &Client,
    session: &mut Session,
    application: &str,
    server_ip: Ipv4Addr,
    clock: RtpClock,
) -> Result<(), Error> {
    let local_ip = super::external_audio::local_media_ip(server_ip)?;
    let mut resources = Resources::new(local_ip)?;
    let addresses = [
        resources.peers[0].socket.local_addr()?,
        resources.peers[0].socket.control_addr()?,
        resources.peers[1].socket.local_addr()?,
        resources.peers[1].socket.control_addr()?,
    ];
    let operation = tokio::time::timeout(
        Duration::from_secs(10),
        resources.prepare(client, session, application),
    )
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    let operation = match operation {
        Ok(()) => {
            let (stop, stopped) = watch::channel(false);
            let peers = &resources.peers;
            let playback = &resources.playback;
            let playback_lifetime = &mut resources.playback_lifetime;
            let sequence = async {
                let result = tokio::time::timeout(
                    Duration::from_secs(45),
                    exercise(client, session, peers, clock, playback, playback_lifetime),
                )
                .await
                .map_err(|_| Error::Timeout)
                .and_then(|result| result);
                stop.send_replace(true);
                result
            };
            let (pumped, observed) = tokio::join!(pump(peers, stopped), sequence);
            combine(observed, pumped)
        }
        Err(error) => Err(error),
    };
    let operation = combine(operation, resources.cleanup(client, session).await);
    drop(resources);
    let mut result = operation;
    for address in addresses {
        result = combine(
            result,
            bounded(async {
                tokio::net::UdpSocket::bind(address)
                    .await
                    .map(drop)
                    .map_err(Error::from)
            })
            .await,
        );
    }
    result
}

#[test]
fn received_silence_cannot_hide_later_music_or_count_duplicate_rtp_as_a_loop() -> Result<(), Error>
{
    let first =
        RtpPacket::new(0, 0, 0, 77, &[rusterisk_media::mulaw_encode(0); 160]).map_err(invalid)?;
    for second in [
        RtpPacket::new(0, 1, 160, 77, &[rusterisk_media::mulaw_encode(1200); 160]),
        RtpPacket::new(0, 0, 0, 77, &[rusterisk_media::mulaw_encode(0); 160]),
        RtpPacket::new(0, 1, 160, 78, &[rusterisk_media::mulaw_encode(0); 160]),
        RtpPacket::new(
            0,
            u16::MAX,
            160,
            77,
            &[rusterisk_media::mulaw_encode(0); 160],
        ),
        RtpPacket::new(0, 2, 320, 77, &[rusterisk_media::mulaw_encode(0); 160]),
        RtpPacket::new(0, 1, 80, 77, &[rusterisk_media::mulaw_encode(0); 160]),
        RtpPacket::new(
            0,
            1,
            0_u32.wrapping_sub(160),
            77,
            &[rusterisk_media::mulaw_encode(0); 160],
        ),
    ] {
        let mut window = AudioWindow::new(0, RtpClock::FirstSample);
        require(
            !window.complete(12, Instant::now()),
            "Missing RTP became received silence",
        )?;
        window.observe(&first, Instant::now())?;
        require(
            matches!(
                window.observe(&second.map_err(invalid)?, Instant::now()),
                Err(Error::Invalid(_))
            ),
            "Forbidden PCM or a discontinuous RTP source entered the audio window",
        )?;
    }
    Ok(())
}

fn known_packet(sequence: u16, timestamp: u32) -> Result<RtpPacket, Error> {
    RtpPacket::new(
        0,
        sequence,
        timestamp,
        77,
        &[rusterisk_media::mulaw_encode(1200); 160],
    )
    .map_err(invalid)
}

#[test]
fn short_pcmu_payloads_contribute_only_their_received_samples() -> Result<(), Error> {
    let start = Instant::now();
    for (clock, short_at, other_clock) in [
        (RtpClock::FirstSample, 160, RtpClock::Asterisk),
        (RtpClock::Asterisk, 138, RtpClock::FirstSample),
    ] {
        let short = RtpPacket::new(
            0,
            1,
            short_at,
            77,
            &[rusterisk_media::mulaw_encode(1200); 138],
        )
        .map_err(invalid)?;
        let mut window = AudioWindow::new(1200, clock);
        window.observe(&known_packet(0, 0)?, start)?;
        window.observe(&short, start + Duration::from_millis(20))?;
        require(
            !window.complete(2, start + Duration::from_millis(20)),
            "A short PCMU payload counted as an entire 20 ms of received audio",
        )?;
        window.observe(&known_packet(2, 298)?, start + Duration::from_millis(38))?;
        require(
            window.complete(2, start + Duration::from_millis(38)),
            "Continuous PCMU with varied payload durations did not accumulate received audio",
        )?;
        let mut other = AudioWindow::new(1200, other_clock);
        other.observe(&known_packet(0, 0)?, start)?;
        require(
            matches!(other.observe(&short, start), Err(Error::Invalid(_))),
            "A PCMU timestamp was accepted under the wrong clock",
        )?;
    }
    Ok(())
}

#[test]
fn legal_presentation_gaps_require_a_fresh_continuous_audio_window() -> Result<(), Error> {
    for gap in [320_u32, 480] {
        for (sequence, timestamp) in [
            (0_u16, 0_u32),
            (u16::MAX.wrapping_sub(58), u32::MAX.wrapping_sub(9360)),
        ] {
            let started = Instant::now();
            let mut window = AudioWindow::new(1200, RtpClock::FirstSample);
            for index in 0..59_u16 {
                let packet = known_packet(
                    sequence.wrapping_add(index),
                    timestamp.wrapping_add(u32::from(index) * 160),
                )?;
                window.observe(
                    &packet,
                    started + Duration::from_millis(u64::from(index) * 20),
                )?;
            }
            let post_repeat = Some((window.total_samples, started + Duration::from_millis(1160)));
            let gap_at = started + Duration::from_millis(1160 + u64::from(gap / 8));
            let gap_timestamp = timestamp.wrapping_add(58 * 160).wrapping_add(gap);
            window.observe(
                &known_packet(sequence.wrapping_add(59), gap_timestamp)?,
                gap_at,
            )?;
            require(
                window.samples == 160
                    && window.total_samples == 60 * 160
                    && !window.complete(60, gap_at),
                "A presentation gap credited old frames to a fresh audio window",
            )?;
            for index in 1..60_u16 {
                let packet = known_packet(
                    sequence.wrapping_add(59).wrapping_add(index),
                    gap_timestamp.wrapping_add(u32::from(index) * 160),
                )?;
                let elapsed = (u64::from(index) * 20).min(1179);
                let now = gap_at + Duration::from_millis(elapsed);
                window.observe(&packet, now)?;
                if index == 11 {
                    require(
                        repeat_complete(window.total_samples, true, post_repeat, now)
                            && !window.complete(60, now),
                        "Post-repeat progress credited earlier frames to a fresh loop",
                    )?;
                }
            }
            require(
                !window.complete(60, gap_at + Duration::from_millis(1179))
                    && window.complete(60, gap_at + Duration::from_millis(1180)),
                "A fresh audio window changed its frame or received-duration requirement",
            )?;
        }
    }
    Ok(())
}

#[test]
fn repeat_completion_keeps_its_response_frame_and_time_baselines() -> Result<(), Error> {
    let responded = Instant::now();
    let post_repeat = Some((59 * 160, responded));
    for (observed, elapsed, expected) in [
        (71 * 160 - 1, 220, false),
        (71 * 160, 219, false),
        (71 * 160, 220, true),
    ] {
        require(
            repeat_complete(
                observed,
                true,
                post_repeat,
                responded + Duration::from_millis(elapsed),
            ) == expected,
            "Repeat completion changed its response baseline of 12 frames or 220 ms",
        )?;
    }
    require(
        !repeat_complete(71, false, None, responded + Duration::from_secs(1))
            && repeat_complete(0, true, None, responded),
        "Repeat completion confused an outstanding request with an absent request",
    )
}
