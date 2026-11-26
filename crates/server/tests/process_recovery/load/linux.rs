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
    time::{Duration, Instant},
};
use tokio::{sync::watch, task::JoinSet};

type Metrics = [u64; 5];

fn sample(pid: u32) -> Result<(u64, u64), ControlError> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(failure)?;
    let (_, fields) = stat
        .rsplit_once(") ")
        .ok_or(ControlError::Invalid("Invalid process statistics"))?;
    let ticks = |index| -> Result<u64, ControlError> {
        fields
            .split_whitespace()
            .nth(index)
            .ok_or(ControlError::Invalid("Missing CPU tick count"))?
            .parse()
            .map_err(failure)
    };
    let status = fs::read_to_string(format!("/proc/{pid}/status")).map_err(failure)?;
    let rss: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
        .ok_or(ControlError::Invalid("Missing resident memory"))?
        .parse()
        .map_err(failure)?;
    Ok((ticks(11)? + ticks(12)?, rss))
}

struct Leg {
    peer: SocketPair,
    target: SocketAddr,
    sender: RtpSender,
    signal: i16,
    heard: Option<Duration>,
}

impl Leg {
    async fn open(client: &Client, server: SocketAddr, n: u8) -> Result<Self, ControlError> {
        let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
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
        Ok(Self {
            peer,
            target: format!("{ip}:{port}").parse().map_err(failure)?,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [n + 1; 10]).map_err(failure)?,
            signal: if n.is_multiple_of(2) { 4000 } else { -7000 },
            heard: None,
        })
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
        start: Instant,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), ControlError> {
        let expected = if self.signal > 0 { -7000 } else { 4000 };
        let mut buffer = [0; 1501];
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.changed() => return Ok(()),
                _ = clock.tick() => self.transmit().await?,
                received = self.peer.data().recv_from(&mut buffer) => {
                    let (length, _) = received.map_err(failure)?;
                    let received_at = start.elapsed();
                    let packet = RtpPacket::parse(&buffer[..length]).map_err(failure)?;
                    if self.heard.is_none()
                        && packet.payload_type() == 0
                        && packet.payload().len() == 160
                        && packet.payload().iter().all(|sample| {
                            (i32::from(mulaw_decode(*sample)) - expected).abs() < 300
                        })
                    {
                        self.heard = Some(received_at);
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
            let start = Instant::now();
            let mut left = Leg::open(&client, addr, 2 * pair).await?;
            let mut right = Leg::open(&client, addr, 2 * pair + 1).await?;
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
            let lifetime = stop.clone();
            calls.spawn(async move {
                tokio::try_join!(
                    left.drive(start, lifetime.clone()),
                    right.drive(start, lifetime)
                )?;
                Ok::<_, ControlError>((pair, [left, right]))
            });
        }
        let mut sample_deadlines = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(300),
            Duration::from_secs(1),
        );
        sample_deadlines.tick().await;
        let start_ticks = sample(pid)?.0;
        sample_deadlines.tick().await;
        let (end_ticks, active_rss) = sample(pid)?;
        let cpu_ticks = end_ticks - start_ticks;
        done.send_replace(true);
        let mut completed = Vec::with_capacity(usize::from(count / 2));
        while let Some(call) = calls.join_next().await {
            completed.push(call.map_err(failure)??);
        }
        completed.sort_unstable_by_key(|(pair, _)| *pair);
        let legs: Vec<_> = completed.into_iter().flat_map(|(_, pair)| pair).collect();
        let mut audio_us = [0; 2];
        let mut release = tokio::time::interval(Duration::from_millis(20));
        release.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        for (n, leg) in legs.iter().enumerate() {
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
            let heard = leg
                .heard
                .ok_or(ControlError::Invalid("Missing received audio at load"))?;
            let micros = u64::try_from(heard.as_micros()).map_err(failure)?;
            audio_us[n % 2] = audio_us[n % 2].max(micros);
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
            cpu_ticks,
            active_rss,
            sample(pid)?.1,
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
    if result.is_err() {
        match rusterisk_test_support::child_exit(&mut server.child, Wait::Responsive) {
            Ok(status) => eprintln!("Server exited during the trial: {status}"),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                eprintln!("Server still running after the failed request")
            }
            Err(error) => return Err(failure(error)),
        }
        for name in ["server.0.log", "server.0.stderr"] {
            let messages = fs::read_to_string(tmp.path().join(name)).map_err(failure)?;
            for line in messages.lines().rev().take(5) {
                eprintln!("{line}");
            }
        }
    }
    let cleanup = server.terminate().await.map(|_| ());
    combine(result, cleanup)
}

pub(super) async fn compare() -> Result<(), ControlError> {
    let quota = fs::read_to_string("/sys/fs/cgroup/cpu.max").map_err(failure)?;
    if quota.trim() != "200000 100000" {
        return Err(ControlError::Invalid("Load comparison requires two CPUs"));
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
                println!("legs={count} round={round} build={build} cpu_ticks={} rss_kib={} released_kib={} left_us={} right_us={}", result[0], result[1], result[2], result[3], result[4]);
                *row = result;
            }
        }
        for (index, label) in [
            "cpu_ticks",
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
