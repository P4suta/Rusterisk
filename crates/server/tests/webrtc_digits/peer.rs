use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_config::{Codec, Config};
use rusterisk_core::{DtmfTone, PeerMediaDirection};
use rusterisk_media::{Frame, MediaError};
use rusterisk_server::{
    media_link::{Channels, DigitCommand},
    rtp::SocketPair,
    webrtc::WebRtcSession,
};
use rusterisk_test_support::Wait;
use std::{net::Ipv4Addr, time::Duration, time::Instant};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch},
};

#[derive(Clone, Copy)]
pub(super) enum Clock {
    Matching,
    Independent,
}

pub(super) struct Application {
    pub audio: mpsc::Receiver<Frame>,
    pub outgoing: mpsc::Sender<Frame>,
    pub digits: mpsc::Receiver<DtmfTone>,
    pub commands: mpsc::Sender<DigitCommand>,
    pub stop: watch::Sender<bool>,
    _direction: watch::Sender<PeerMediaDirection>,
}

pub(super) fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

pub(super) fn channels() -> (Channels, Application) {
    let (incoming, audio) = mpsc::channel(3);
    let (outgoing, output) = mpsc::channel(3);
    let (digits, received) = mpsc::channel(16);
    let (commands, pending) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (directed, direction) = watch::channel(PeerMediaDirection::SendReceive);
    (
        Channels {
            incoming,
            digits,
            outgoing: output,
            commands: pending,
            completed,
            stop: stopped,
            direction,
        },
        Application {
            audio,
            outgoing,
            digits: received,
            commands,
            stop,
            _direction: directed,
        },
    )
}

pub(super) fn configuration(peers: &[UdpSocket; 2], clock: Clock) -> Result<Config, MediaError> {
    let allow = match clock {
        Clock::Matching => "ulaw",
        Clock::Independent => "opus,ulaw",
    };
    let mut sip = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n".to_owned();
    for (name, peer) in ["left", "right"].into_iter().zip(peers) {
        sip.push_str(&format!("[{name}-aor]\ntype=aor\ncontact=sip:phone@{}\n[{name}]\ntype=endpoint\ntransport=udp\nauth=auth\naors={name}-aor\nallow={allow}\nwebrtc=yes\ndtmf_mode=rfc4733\ncontext=internal\n", peer.local_addr().map_err(failure)?));
    }
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Stasis(calls)\n",
    )
    .map_err(failure)?;
    config.rtp.start = 0;
    config.rtp.end = 0;
    Ok(config)
}

fn eight_khz_only(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    for line in source.lines() {
        if line.starts_with("a=rtpmap:102 ") || line.starts_with("a=fmtp:102 ") {
            continue;
        }
        if line.starts_with("m=audio ") {
            let fields: Vec<_> = line
                .split_whitespace()
                .filter(|field| *field != "102")
                .collect();
            output.push_str(&fields.join(" "));
        } else {
            output.push_str(line);
        }
        output.push_str("\r\n");
    }
    output
}

pub(super) async fn answer(signal: &UdpSocket, clock: Clock) -> Result<WebRtcSession, MediaError> {
    let mut bytes = [0; 16385];
    let (size, server) = Wait::Ready
        .bounded(signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
    else {
        return Err(MediaError::InvalidPacket);
    };
    if request.method != Method::Invite {
        return Err(MediaError::InvalidPacket);
    }
    let offered = std::str::from_utf8(&request.body).map_err(failure)?;
    let (codec, offered) = match clock {
        Clock::Matching => (Codec::Pcmu, offered.to_owned()),
        Clock::Independent => (Codec::Opus, eight_khz_only(offered)),
    };
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let (session, body) =
        WebRtcSession::answer(sockets, local, &offered, &[codec], Instant::now())?;
    let selected = rusterisk_sip::Session::parse(&body, &[codec]).map_err(failure)?;
    if selected.codec != codec || session.telephone().ok_or(MediaError::Unsupported)?.rate() != 8000
    {
        return Err(MediaError::Unsupported);
    }
    let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=digits\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value(), signal.local_addr().map_err(failure)?, body.len());
    signal
        .send_to(response.as_bytes(), server)
        .await
        .map_err(failure)?;
    Ok(session)
}

pub(super) async fn finish<T, E: std::fmt::Display>(
    mut task: tokio::task::JoinHandle<Result<T, E>>,
) -> Result<T, MediaError> {
    match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    }
}

pub(super) fn combine(
    primary: Result<(), MediaError>,
    cleanup: Result<(), MediaError>,
) -> Result<(), MediaError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Err(cleanup)) => {
            Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
    }
}

fn signal(frequency: f64, frame: usize) -> Result<Frame, MediaError> {
    let mut samples = [0; 960];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = (6000.0
            * (std::f64::consts::TAU * frequency * (frame * 960 + index) as f64 / 48000.0).sin())
            as i16;
    }
    Frame::new(&samples, 48000)
}

fn energy(frame: &Frame, frequency: f64) -> f64 {
    let (mut real, mut imaginary) = (0.0, 0.0);
    for (index, sample) in frame.samples().iter().enumerate() {
        let phase = std::f64::consts::TAU * frequency * index as f64 / f64::from(frame.rate());
        real += f64::from(*sample) * phase.cos();
        imaginary += f64::from(*sample) * phase.sin();
    }
    (real * real + imaginary * imaginary) / frame.samples().len().pow(2) as f64
}

fn recognized(frame: &Frame, expected: f64, refused: f64) -> bool {
    let wanted = energy(frame, expected);
    wanted > 100000.0 && wanted > energy(frame, refused) * 10.0
}

pub(super) async fn audio(
    left: &mut Application,
    right: &mut Application,
) -> Result<(), MediaError> {
    let mut received = [false; 2];
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = 0;
    while sent < 20 || !received.iter().all(|received| *received) {
        tokio::select! {
            _instant = clock.tick(), if sent < 20 => {
                left.outgoing.try_send(signal(440.0, sent)?).map_err(failure)?;
                right.outgoing.try_send(signal(660.0, sent)?).map_err(failure)?;
                sent += 1;
            }
            frame = left.audio.recv() => received[0] |= recognized(&frame.ok_or(MediaError::UnknownStream)?, 660.0, 440.0),
            frame = right.audio.recv() => received[1] |= recognized(&frame.ok_or(MediaError::UnknownStream)?, 440.0, 660.0),
        }
    }
    Ok(())
}
