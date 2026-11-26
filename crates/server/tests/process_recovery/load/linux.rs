use super::super::{
    combine, events, failure, request, Channel, ChildServer, ControlError, SocketPair,
};
use futures_util::StreamExt;
use reqwest::{Client, Method};
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_test_support::Wait;
use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::watch, task::JoinSet};

type Metrics = [u64; 5];

#[derive(Default)]
struct ReceivedAudio {
    first: Option<Duration>,
    last: Duration,
    longest_gap: Duration,
}

impl ReceivedAudio {
    fn hear(&mut self, at: Duration) -> Result<(), ControlError> {
        if self.first.is_some() {
            let gap = at
                .checked_sub(self.last)
                .ok_or(ControlError::Invalid("Packet clock moved backward"))?;
            self.longest_gap = self.longest_gap.max(gap);
        } else {
            self.first = Some(at);
        }
        self.last = at;
        Ok(())
    }

    fn first_if_continuous(&self, end: Duration) -> Option<Duration> {
        let gap = self.longest_gap.max(end.checked_sub(self.last)?);
        self.first.filter(|first| {
            self.last.saturating_sub(*first) >= Duration::from_secs(1)
                && gap <= Wait::Quiet.duration()
        })
    }
}

#[test]
fn a_burst_or_an_audio_gap_cannot_validate_a_second_of_load() -> Result<(), ControlError> {
    let mut audio = ReceivedAudio::default();
    let end = Duration::from_millis(1100);
    assert_eq!(audio.first_if_continuous(end), None);
    for n in 1..=5 {
        audio.hear(Duration::from_millis(n * 20))?;
    }
    assert_eq!(audio.first_if_continuous(end), None);
    audio.hear(end)?;
    assert_eq!(audio.first_if_continuous(end), None);
    let mut audio = ReceivedAudio::default();
    for n in 1..=55 {
        audio.hear(Duration::from_millis(n * 20))?;
    }
    assert_eq!(
        audio.first_if_continuous(end),
        Some(Duration::from_millis(20))
    );
    Ok(())
}

#[test]
fn backward_packet_or_shutdown_timestamps_reject_the_load_sample() -> Result<(), ControlError> {
    let mut audio = ReceivedAudio::default();
    for n in 1..=55 {
        audio.hear(Duration::from_millis(n * 20))?;
    }
    assert_eq!(audio.first_if_continuous(Duration::from_millis(1099)), None);
    assert!(audio.hear(Duration::from_millis(1050)).is_err());
    Ok(())
}

#[test]
fn the_process_cpu_clock_observes_work_shorter_than_a_scheduler_tick() -> Result<(), ControlError> {
    let pid = std::process::id();
    let start = process_cpu_ns(pid)?;
    for n in 0_u64..20_000 {
        std::hint::black_box(n.wrapping_mul(73));
    }
    let elapsed = process_cpu_ns(pid)? - start;
    assert!(elapsed > 0);
    assert_ne!(elapsed % 10_000_000, 0);
    Ok(())
}

fn process_cpu_ns(pid: u32) -> Result<u64, ControlError> {
    use nix::{sys::time::TimeValLike, time::ClockId, unistd::Pid};
    let pid = Pid::from_raw(i32::try_from(pid).map_err(failure)?);
    let clock = ClockId::pid_cpu_clock_id(pid).map_err(failure)?;
    clock
        .now()
        .map_err(failure)?
        .num_nanoseconds()
        .try_into()
        .map_err(failure)
}

fn resident_kib(pid: u32) -> Result<u64, ControlError> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).map_err(failure)?;
    let rss: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
        .ok_or(ControlError::Invalid("Missing resident memory"))?
        .parse()
        .map_err(failure)?;
    Ok(rss)
}

struct Leg {
    peer: SocketPair,
    timestamps: Vec<u8>,
    target: SocketAddr,
    sender: RtpSender,
    next_packet: tokio::time::Instant,
    signal: i16,
}

#[tokio::test]
async fn a_negotiated_phone_sends_audio_before_its_bridge_exists() -> Result<(), ControlError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let receiver = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(failure)?;
    receiver.set_nonblocking(true).map_err(failure)?;
    let target = receiver.local_addr().map_err(failure)?;
    let leg = Leg::negotiated(peer, target, 0).await?;
    let mut bytes = [0; 1500];
    let (length, source) = receiver.recv_from(&mut bytes).map_err(failure)?;
    assert_eq!(source, leg.peer.local_addr().map_err(failure)?);
    let packet = RtpPacket::parse(&bytes[..length]).map_err(failure)?;
    assert_eq!(packet.payload(), [rusterisk_media::mulaw_encode(4000); 160]);
    Ok(())
}

#[tokio::test]
async fn first_audio_is_timestamped_before_a_ready_packet_is_read() -> Result<(), ControlError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let address = peer.local_addr().map_err(failure)?;
    let sender = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let mut leg = Leg::negotiated(peer, sender.local_addr().map_err(failure)?, 0).await?;
    sender
        .send_to(b"received", address)
        .await
        .map_err(failure)?;
    leg.peer.data().readable().await.map_err(failure)?;
    let ready = SystemTime::now();
    let mut bytes = [0; 16];
    let (length, arrival) = leg.receive(&mut bytes).await?;
    assert_eq!(&bytes[..length], b"received");
    assert!(
        arrival <= ready,
        "the receiver included time spent waiting in its socket"
    );
    Ok(())
}

impl Leg {
    async fn receive(&mut self, buffer: &mut [u8]) -> Result<(usize, SystemTime), ControlError> {
        use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};
        use std::{io::IoSliceMut, os::fd::AsRawFd};
        let socket = self.peer.data();
        socket
            .async_io(tokio::io::Interest::READABLE, || {
                let mut slices = [IoSliceMut::new(buffer)];
                let packet = recvmsg::<()>(
                    socket.as_raw_fd(),
                    &mut slices,
                    Some(&mut self.timestamps),
                    MsgFlags::empty(),
                )?;
                let arrival = packet
                    .cmsgs()?
                    .find_map(|message| match message {
                        ControlMessageOwned::ScmTimestampns(time) => {
                            UNIX_EPOCH.checked_add(time.into())
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Missing packet arrival timestamp",
                        )
                    })?;
                Ok((packet.bytes, arrival))
            })
            .await
            .map_err(failure)
    }

    async fn open(
        peer: SocketPair,
        client: &Client,
        server: SocketAddr,
        n: u8,
    ) -> Result<Self, ControlError> {
        let name = format!("load-{n}");
        let remote = peer.local_addr().map_err(failure)?.to_string();
        let channel: Channel = request(
            client,
            server,
            Method::POST,
            "channels/externalMedia",
            &[
                ("app", "recovery"),
                ("channelId", &name),
                ("external_host", &remote),
                ("format", "ulaw"),
                ("transport", "udp"),
                ("encapsulation", "rtp"),
            ],
        )
        .await?
        .json()
        .await
        .map_err(failure)?;
        let vars = channel
            .channelvars
            .ok_or(ControlError::Invalid("External channel has no variables"))?;
        let ip = vars
            .get("UNICASTRTP_LOCAL_ADDRESS")
            .ok_or(ControlError::Invalid("Missing RTP address"))?;
        let port = vars
            .get("UNICASTRTP_LOCAL_PORT")
            .ok_or(ControlError::Invalid("Missing RTP port"))?;
        Self::negotiated(peer, format!("{ip}:{port}").parse().map_err(failure)?, n).await
    }

    async fn negotiated(peer: SocketPair, target: SocketAddr, n: u8) -> Result<Self, ControlError> {
        nix::sys::socket::setsockopt(
            peer.data(),
            nix::sys::socket::sockopt::ReceiveTimestampns,
            &true,
        )
        .map_err(failure)?;
        let mut leg = Self {
            peer,
            timestamps: nix::cmsg_space!(nix::sys::time::TimeSpec),
            target,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [n + 1; 10]).map_err(failure)?,
            next_packet: tokio::time::Instant::now() + Duration::from_millis(20),
            signal: if n.is_multiple_of(2) { 4000 } else { -7000 },
        };
        leg.transmit().await?;
        Ok(leg)
    }

    async fn transmit(&mut self) -> Result<(), ControlError> {
        let frame = Frame::new(&[self.signal; 160], 8000).map_err(failure)?;
        let data = self
            .sender
            .packet(frame)
            .map_err(failure)?
            .encode()
            .map_err(failure)?;
        self.peer
            .data()
            .send_to(&data, self.target)
            .await
            .map_err(failure)?;
        Ok(())
    }

    async fn drive(
        &mut self,
        start: SystemTime,
        mut stop: watch::Receiver<bool>,
    ) -> Result<Duration, ControlError> {
        let expected = if self.signal > 0 { -7000 } else { 4000 };
        let mut audio = ReceivedAudio::default();
        let mut buffer = [0; 1501];
        let mut clock = tokio::time::interval_at(self.next_packet, Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.changed() => return audio.first_if_continuous(start.elapsed().map_err(failure)?)
                    .ok_or(ControlError::Invalid("Peer did not receive continuous audio at load")),
                _ = clock.tick() => self.transmit().await?,
                received = self.receive(&mut buffer) => {
                    let (length, arrived) = received?;
                    let received_at = arrived.duration_since(start).map_err(failure)?;
                    let packet = RtpPacket::parse(&buffer[..length]).map_err(failure)?;
                    if packet.payload_type() == 0
                        && packet.payload().len() == 160
                        && packet.payload().iter().all(|sample| {
                            (i32::from(mulaw_decode(*sample)) - expected).abs() < 300
                        })
                    {
                        audio.hear(received_at)?;
                    }
                }
            }
        }
    }
}

async fn measure(process: &ChildServer, count: u8) -> Result<Metrics, ControlError> {
    let addr = process.address();
    let pid = process.child.id();
    let client = Client::builder()
        .no_proxy()
        .timeout(Wait::Io.duration())
        .build()
        .map_err(failure)?;
    let mut socket = events(addr).await?;
    let (stop, mut ends) = tokio::sync::oneshot::channel::<()>();
    let reader = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut ends => return Ok(()),
                event = socket.next() => match event {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(failure(error)),
                    None => return Err(ControlError::Invalid("Application event connection closed")),
                }
            }
        }
    });
    let result = async {
        let mut calls = JoinSet::new();
        let (done, stop) = watch::channel(false);
        for pair in 0..count / 2 {
            let bind = || SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure);
            let phones = [(2 * pair, bind()?), (2 * pair + 1, bind()?)];
            let start = SystemTime::now();
            for (n, peer) in phones {
                let mut leg = Leg::open(peer, &client, addr, n).await?;
                let lifetime = stop.clone();
                calls.spawn(async move {
                    let first = leg.drive(start, lifetime).await?;
                    Ok::<_, ControlError>((n, leg, first))
                });
            }
            let bridge = format!("bridges/pair-{pair}");
            request(&client, addr, Method::POST, &bridge, &[("type", "mixing")]).await?;
            request(
                &client,
                addr,
                Method::POST,
                &format!("{bridge}/addChannel"),
                &[(
                    "channel",
                    &format!("load-{},load-{}", 2 * pair, 2 * pair + 1),
                )],
            )
            .await?;
        }
        let mut sample_deadlines = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(300),
            Duration::from_secs(1),
        );
        sample_deadlines.tick().await;
        let wall_start = Instant::now();
        let cpu_start = process_cpu_ns(pid)?;
        sample_deadlines.tick().await;
        let cpu_ns = super::cpu_per_second(cpu_start, process_cpu_ns(pid)?, wall_start.elapsed())
            .ok_or(ControlError::Invalid(
            "Invalid process CPU sampling interval",
        ))?;
        let active_rss = resident_kib(pid)?;
        done.send_replace(true);
        let mut completed = Vec::with_capacity(usize::from(count));
        while let Some(call) = calls.join_next().await {
            completed.push(call.map_err(failure)??);
        }
        completed.sort_unstable_by_key(|(n, _, _)| *n);
        let mut audio_us = [0; 2];
        let mut release = tokio::time::interval(Duration::from_millis(20));
        release.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        for (n, leg, heard) in &completed {
            if n % 2 == 0 {
                release.tick().await;
                request(
                    &client,
                    addr,
                    Method::DELETE,
                    &format!("bridges/pair-{}", n / 2),
                    &[],
                )
                .await?;
            }
            let micros = u64::try_from(heard.as_micros()).map_err(failure)?;
            let direction = usize::from(n % 2);
            audio_us[direction] = audio_us[direction].max(micros);
            let path = format!("channels/load-{n}");
            Wait::Ready
                .bounded(async {
                    let mut admission = tokio::time::interval(Duration::from_millis(20));
                    loop {
                        admission.tick().await;
                        match request(&client, addr, Method::DELETE, &path, &[]).await {
                            Err(ControlError::Capacity) => continue,
                            outcome => return outcome.map(|_| ()),
                        }
                    }
                })
                .await
                .map_err(|error| failure(format!("hangup {n}: {error}")))??;
            Wait::Ready
                .bounded(async {
                    let mut observation = tokio::time::interval(Duration::from_millis(10));
                    loop {
                        match std::net::UdpSocket::bind(leg.target) {
                            Ok(socket) => {
                                drop(socket);
                                return Ok::<(), ControlError>(());
                            }
                            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                                observation.tick().await;
                            }
                            Err(error) => return Err(failure(error)),
                        }
                    }
                })
                .await
                .map_err(failure)??;
        }
        let remaining: Vec<Channel> = request(&client, addr, Method::GET, "channels", &[])
            .await?
            .json()
            .await
            .map_err(failure)?;
        if !remaining.is_empty() {
            return Err(ControlError::Invalid("Load calls survived hangup"));
        }
        Ok([
            cpu_ns,
            active_rss,
            resident_kib(pid)?,
            audio_us[0],
            audio_us[1],
        ])
    }
    .await;
    drop(stop);
    let stream = reader.await.map_err(failure).and_then(|outcome| outcome);
    combine(result, stream)
}

async fn trial(binary: &Path, count: u8) -> Result<Metrics, ControlError> {
    let tmp = tempfile::tempdir().map_err(failure)?;
    for (file, value) in [
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        ("pjsip.conf", ""),
        ("rtp.conf", ""),
        ("extensions.conf", ""),
    ] {
        fs::write(tmp.path().join(file), value).map_err(failure)?;
    }
    let mut server = ChildServer::start_from(
        binary,
        tmp.path(),
        &tmp.path().join("data"),
        tmp.path().join("server.log"),
    )
    .await?;
    let result = Wait::Scenario
        .bounded(measure(&server, count))
        .await
        .map_err(failure)
        .and_then(|result| result);
    super::finish_trial(&mut server, tmp.path(), result)
}

pub(super) async fn compare() -> Result<(), ControlError> {
    let quota = fs::read_to_string("/sys/fs/cgroup/cpu.max").map_err(failure)?;
    let status = fs::read_to_string("/proc/self/status").map_err(failure)?;
    if !super::two_cpu_profile(&quota, &status) {
        return Err(ControlError::Invalid(
            "Load comparison requires a two-CPU quota and affinity",
        ));
    }
    let binary = |variable| {
        std::env::var_os(variable)
            .map(PathBuf::from)
            .ok_or(ControlError::Invalid("Missing packaged server"))
    };
    let executables = [
        binary("RUSTERISK_BASELINE_SERVER")?,
        binary("RUSTERISK_CANDIDATE_SERVER")?,
    ];
    let mut regressions = Vec::new();
    for count in [64, 0, 2, 16] {
        let mut samples = [[[0; 5]; 2]; 5];
        for (round, rows) in samples.iter_mut().enumerate() {
            for (build, (row, binary)) in rows.iter_mut().zip(&executables).enumerate() {
                let result = trial(binary, count).await.map_err(|error| {
                    failure(format!(
                        "Load {count}, round {round}, build {build}: {error}"
                    ))
                })?;
                println!("legs={count} round={round} build={build} cpu_ns_per_second={} rss_kib={} released_kib={} left_us={} right_us={}", result[0], result[1], result[2], result[3], result[4]);
                *row = result;
            }
        }
        for (index, label) in [
            "cpu_ns_per_second",
            "rss_kib",
            "released_kib",
            "left_us",
            "right_us",
        ]
        .iter()
        .enumerate()
        {
            let baseline = samples.map(|row| row[0][index]);
            let candidate = samples.map(|row| row[1][index]);
            if cfg!(target_arch = "aarch64")
                && matches!(*label, "rss_kib" | "released_kib")
                && !super::within_arm64_rss(count, candidate)
            {
                regressions.push(format!(
                    "{count} legs {label} exceeded the ARM64 memory budget: {candidate:?}"
                ));
            }
            if !super::within_baseline(baseline, candidate) {
                regressions.push(format!(
                    "{count} legs {label}: baseline {baseline:?}, candidate {candidate:?}"
                ));
            }
        }
    }
    if regressions.is_empty() {
        Ok(())
    } else {
        Err(failure(regressions.join("\n")))
    }
}
