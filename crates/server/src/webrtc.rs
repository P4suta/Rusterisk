use crate::rtp::SocketPair;
use base64::Engine as Base64Engine;
use rusterisk_config::Codec;
use rusterisk_media::MediaError;
use std::{fmt, net::SocketAddr, time::Instant};
use str0m::{
    change::{SdpAnswer, SdpOffer, SdpPendingOffer},
    media::{Direction, MediaKind, Mid, Pt},
    net::Transmit,
    Candidate, Event, IceCreds, Output, Rtc, RtcConfig,
};
mod description;
mod link;
pub use link::WebRtcStats;

struct Engine {
    rtc: Rtc,
    sockets: SocketPair,
    mid: Option<Mid>,
    transmits: Vec<Transmit>,
    timeout: Instant,
    tls_id: String,
}

pub struct WebRtcOffer {
    engine: Engine,
    pending: SdpPendingOffer,
    description: String,
    allowed: Vec<Codec>,
}

pub struct WebRtcSession {
    engine: Engine,
    codec: Codec,
    payload: Pt,
}

fn negotiation_error(_error: impl fmt::Display) -> MediaError {
    MediaError::Transport("WebRTC negotiation failed".into())
}

fn checked_description(source: &str) -> Result<rusterisk_sip::WebRtcDescription, MediaError> {
    rusterisk_sip::WebRtcDescription::parse(source).map_err(|error| match error {
        rusterisk_sip::SipError::Capacity => MediaError::Capacity,
        rusterisk_sip::SipError::Unsupported => MediaError::Unsupported,
        rusterisk_sip::SipError::InvalidPacket
        | rusterisk_sip::SipError::Entropy
        | rusterisk_sip::SipError::Tag
        | rusterisk_sip::SipError::Config(_)
        | rusterisk_sip::SipError::Parse(_)
        | rusterisk_sip::SipError::Address(_)
        | rusterisk_sip::SipError::Utf8(_)
        | rusterisk_sip::SipError::Io(_) => MediaError::InvalidPacket,
    })
}

fn matches(codec: Codec, actual: str0m::format::Codec) -> bool {
    match codec {
        Codec::Pcmu => actual == str0m::format::Codec::PCMU,
        Codec::Pcma => actual == str0m::format::Codec::PCMA,
        Codec::Opus => actual == str0m::format::Codec::Opus,
    }
}

impl Engine {
    fn new(
        sockets: SocketPair,
        advertised: SocketAddr,
        allowed: &[Codec],
        now: Instant,
    ) -> Result<Self, MediaError> {
        let local = sockets.local_addr().map_err(negotiation_error)?;
        if allowed.is_empty()
            || allowed.len() > 3
            || advertised.port() == 0
            || advertised.ip().is_unspecified()
            || advertised.ip().is_multicast()
            || local.ip().is_unspecified()
            || local.ip().is_multicast()
        {
            return Err(MediaError::InvalidRoute);
        }
        let certificate = str0m::crypto::from_feature_flags()
            .dtls_provider
            .generate_certificate()
            .ok_or_else(|| MediaError::Transport("WebRTC certificate generation failed".into()))?;
        let mut entropy = [0; 56];
        getrandom::fill(&mut entropy)
            .map_err(|_| MediaError::Transport("WebRTC entropy unavailable".into()))?;
        let credentials = IceCreds {
            ufrag: base64::engine::general_purpose::STANDARD_NO_PAD.encode(&entropy[..8]),
            pass: base64::engine::general_purpose::STANDARD_NO_PAD.encode(&entropy[8..40]),
        };
        let mut config = RtcConfig::new()
            .clear_codecs()
            .set_local_ice_credentials(credentials)
            .set_dtls_cert(certificate)
            .set_fingerprint_verification(true)
            .set_stats_interval(None)
            .set_reordering_size_audio(16)
            .set_send_buffer_audio(32);
        for codec in allowed {
            config = match codec {
                Codec::Pcmu => config.enable_pcmu(true, false),
                Codec::Pcma => config.enable_pcma(true, false),
                Codec::Opus => config.enable_opus(true, false),
            };
        }
        let mut engine = Self {
            rtc: config.build(now),
            sockets,
            mid: None,
            transmits: Vec::with_capacity(32),
            timeout: now,
            tls_id: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&entropy[40..]),
        };
        engine.drain()?;
        if engine
            .rtc
            .add_local_candidate(Candidate::host(local, "udp").map_err(negotiation_error)?)
            .is_none()
        {
            return Err(MediaError::InvalidRoute);
        }
        engine.drain()?;
        if advertised != local {
            let candidate =
                Candidate::server_reflexive(advertised, local, "udp").map_err(negotiation_error)?;
            if engine.rtc.add_local_candidate(candidate).is_none() {
                return Err(MediaError::InvalidRoute);
            }
            engine.drain()?;
        }
        Ok(engine)
    }

    fn local_description(
        &self,
        mut source: String,
        identify_tls: bool,
    ) -> Result<String, MediaError> {
        if identify_tls {
            source.push_str("a=tls-id:");
            source.push_str(&self.tls_id);
            source.push_str("\r\n");
        }
        checked_description(&source)?;
        Ok(source)
    }

    fn drain(&mut self) -> Result<(), MediaError> {
        for _ in 0..256 {
            match self.rtc.poll_output().map_err(negotiation_error)? {
                Output::Timeout(timeout) => {
                    self.timeout = timeout;
                    return Ok(());
                }
                Output::Transmit(transmit) => {
                    if self.transmits.len() >= 32 || transmit.contents.len() > 2048 {
                        return Err(MediaError::Capacity);
                    }
                    self.transmits.push(transmit);
                }
                Output::Event(event) => {
                    if self.metadata(event)?.is_some() {
                        return Err(MediaError::Unsupported);
                    }
                }
            }
        }
        Err(MediaError::Capacity)
    }

    fn metadata(&mut self, event: Event) -> Result<Option<str0m::media::MediaData>, MediaError> {
        match event {
            Event::MediaAdded(added) => {
                if added.kind != MediaKind::Audio || self.mid.is_some_and(|mid| mid != added.mid) {
                    return Err(MediaError::Unsupported);
                }
                self.mid = Some(added.mid);
            }
            Event::MediaChanged(changed) => {
                if self.mid != Some(changed.mid) {
                    return Err(MediaError::InvalidRoute);
                }
            }
            Event::MediaData(data) => return Ok(Some(data)),
            Event::Connected
            | Event::IceConnectionStateChange(_)
            | Event::PeerStats(_)
            | Event::MediaIngressStats(_)
            | Event::MediaEgressStats(_)
            | Event::EgressBitrateEstimate(_)
            | Event::KeyframeRequest(_)
            | Event::StreamPaused(_)
            | Event::SenderFeedback(_) => {}
            Event::Closed => return Err(MediaError::UnknownStream),
            Event::ChannelOpen(_, _)
            | Event::ChannelData(_)
            | Event::ChannelClose(_)
            | Event::ChannelBufferedAmountLow(_)
            | Event::RtpPacket(_)
            | Event::AppSpecificFeedback(_)
            | Event::RawPacket(_) => return Err(MediaError::Unsupported),
            _ => return Err(MediaError::Unsupported),
        }
        Ok(None)
    }

    fn negotiated(mut self, allowed: &[Codec]) -> Result<WebRtcSession, MediaError> {
        let mid = self.mid.ok_or(MediaError::InvalidRoute)?;
        let selected = {
            let writer = self.rtc.writer(mid).ok_or(MediaError::InvalidRoute)?;
            allowed.iter().find_map(|codec| {
                writer
                    .payload_params()
                    .find(|parameters| matches(*codec, parameters.spec().codec))
                    .map(|parameters| (*codec, parameters.pt()))
            })
        }
        .ok_or(MediaError::Unsupported)?;
        self.drain()?;
        Ok(WebRtcSession {
            engine: self,
            codec: selected.0,
            payload: selected.1,
        })
    }
}

impl WebRtcOffer {
    pub fn new(
        sockets: SocketPair,
        advertised: SocketAddr,
        allowed: &[Codec],
        now: Instant,
    ) -> Result<Self, MediaError> {
        let mut engine = Engine::new(sockets, advertised, allowed, now)?;
        let mut change = engine.rtc.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, pending) = change.apply().ok_or(MediaError::InvalidRoute)?;
        engine.mid = Some(mid);
        engine.drain()?;
        let description = engine.local_description(offer.to_sdp_string(), true)?;
        Ok(Self {
            engine,
            pending,
            description,
            allowed: allowed.to_vec(),
        })
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn accept_answer(mut self, answer: &str) -> Result<WebRtcSession, MediaError> {
        let checked = checked_description(answer)?;
        let answer = SdpAnswer::from_sdp_string(&description::for_transport(&checked))
            .map_err(negotiation_error)?;
        self.engine
            .rtc
            .sdp_api()
            .accept_answer(self.pending, answer)
            .map_err(negotiation_error)?;
        self.engine.drain()?;
        self.engine.negotiated(&self.allowed)
    }
}

impl WebRtcSession {
    pub fn answer(
        sockets: SocketPair,
        advertised: SocketAddr,
        offer: &str,
        allowed: &[Codec],
        now: Instant,
    ) -> Result<(Self, String), MediaError> {
        let checked = checked_description(offer)?;
        let identify_tls = checked.tls_id().is_some();
        let offer = SdpOffer::from_sdp_string(&description::for_transport(&checked))
            .map_err(negotiation_error)?;
        let mut engine = Engine::new(sockets, advertised, allowed, now)?;
        let answer = engine
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(negotiation_error)?
            .to_sdp_string();
        let answer = engine.local_description(answer, identify_tls)?;
        let mut mids = answer
            .lines()
            .filter_map(|line| line.strip_prefix("a=mid:"));
        let mid = mids.next().ok_or(MediaError::InvalidRoute)?;
        if mid.is_empty() || mid.len() > 16 || !mid.is_ascii() || mids.next().is_some() {
            return Err(MediaError::InvalidRoute);
        }
        engine.mid = Some(Mid::from(mid));
        engine.drain()?;
        Ok((engine.negotiated(allowed)?, answer))
    }

    pub fn is_connected(&self) -> bool {
        self.engine.rtc.is_connected()
    }
}

impl fmt::Debug for WebRtcOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebRtcOffer")
            .field("local", &self.engine.sockets.local_addr())
            .field("mid", &self.engine.mid)
            .field("queued_transmits", &self.engine.transmits.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for WebRtcSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebRtcSession")
            .field("local", &self.engine.sockets.local_addr())
            .field("mid", &self.engine.mid)
            .field("codec", &self.codec)
            .field("payload", &self.payload)
            .field("timeout", &self.engine.timeout)
            .finish_non_exhaustive()
    }
}
