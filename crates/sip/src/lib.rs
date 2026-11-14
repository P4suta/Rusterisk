use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::Authorization,
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    HeadersExt, Method, Request, SipMessage,
};
use rusterisk_config::{Codec, Config};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use subtle::ConstantTimeEq;
mod incoming;
mod outgoing;
mod transactions;
pub use incoming::RejectReason;
pub use outgoing::Outbound;

pub const MAX_MESSAGE: usize = 16384;

pub enum SipError {
    InvalidPacket,
    Capacity,
    Unsupported,
    Entropy,
    Config(rusterisk_config::ConfigError),
    Parse(rsipstack::sip::Error),
    Address(std::net::AddrParseError),
    Utf8(std::str::Utf8Error),
    Io(std::io::Error),
}

impl std::fmt::Display for SipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPacket | Self::Parse(_) | Self::Utf8(_) => {
                f.write_str("Invalid signaling packet")
            }
            Self::Capacity => f.write_str("Signaling capacity exceeded"),
            Self::Unsupported => f.write_str("Unsupported signaling or media"),
            Self::Entropy => f.write_str("Signaling entropy unavailable"),
            Self::Config(error) => write!(f, "{error}"),
            Self::Address(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}
impl std::fmt::Debug for SipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for SipError {}
impl From<rusterisk_config::ConfigError> for SipError {
    fn from(error: rusterisk_config::ConfigError) -> Self {
        Self::Config(error)
    }
}
impl From<rsipstack::sip::Error> for SipError {
    fn from(error: rsipstack::sip::Error) -> Self {
        Self::Parse(error)
    }
}
impl From<std::net::AddrParseError> for SipError {
    fn from(error: std::net::AddrParseError) -> Self {
        Self::Address(error)
    }
}
impl From<std::str::Utf8Error> for SipError {
    fn from(error: std::str::Utf8Error) -> Self {
        Self::Utf8(error)
    }
}
impl From<std::io::Error> for SipError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    SendRecv,
    SendOnly,
    RecvOnly,
    Inactive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub remote: SocketAddr,
    pub codec: Codec,
    pub payload_type: u8,
    pub dtmf_payload: Option<u8>,
    pub direction: Direction,
}

impl Session {
    pub fn parse(source: &str, allowed: &[Codec]) -> Result<Self, SipError> {
        if source.len() > MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        let mut address = None;
        let mut port = None;
        let mut payloads: Vec<u8> = Vec::new();
        let mut mappings = BTreeMap::new();
        mappings.insert(0, Codec::Pcmu);
        mappings.insert(8, Codec::Pcma);
        let mut dtmf = None;
        let mut direction = Direction::SendRecv;
        for line in source.lines().map(str::trim) {
            if let Some(connection) = line
                .strip_prefix("c=IN IP4 ")
                .or_else(|| line.strip_prefix("c=IN IP6 "))
            {
                address = Some(connection.parse::<IpAddr>()?);
            } else if let Some(media) = line.strip_prefix("m=") {
                if port.is_some() {
                    return Err(SipError::Unsupported);
                }
                let parts: Vec<&str> = media.split_whitespace().collect();
                let ["audio", number, "RTP/AVP" | "RTP/AVPF", formats @ ..] = parts.as_slice()
                else {
                    return Err(SipError::Unsupported);
                };
                if formats.is_empty() || formats.len() > 16 {
                    return Err(SipError::InvalidPacket);
                }
                port = Some(number.parse::<u16>().map_err(|_| SipError::InvalidPacket)?);
                for format in formats {
                    payloads.push(format.parse().map_err(|_| SipError::InvalidPacket)?);
                }
            } else if let Some(mapping) = line.strip_prefix("a=rtpmap:") {
                let (payload, format) = mapping.split_once(' ').ok_or(SipError::InvalidPacket)?;
                let payload: u8 = payload.parse().map_err(|_| SipError::InvalidPacket)?;
                let lower = format.to_ascii_lowercase();
                match lower.as_str() {
                    "pcmu/8000" | "pcmu/8000/1" => {
                        mappings.insert(payload, Codec::Pcmu);
                    }
                    "pcma/8000" | "pcma/8000/1" => {
                        mappings.insert(payload, Codec::Pcma);
                    }
                    "opus/48000/2" | "opus/48000/1" => {
                        mappings.insert(payload, Codec::Opus);
                    }
                    "telephone-event/8000" => dtmf = Some(payload),
                    _ => {}
                }
            } else {
                match line {
                    "a=sendrecv" => direction = Direction::SendRecv,
                    "a=sendonly" => direction = Direction::SendOnly,
                    "a=recvonly" => direction = Direction::RecvOnly,
                    "a=inactive" => direction = Direction::Inactive,
                    _ => {}
                }
            }
        }
        let remote = SocketAddr::new(
            address.ok_or(SipError::InvalidPacket)?,
            port.filter(|port| *port != 0)
                .ok_or(SipError::InvalidPacket)?,
        );
        for codec in allowed {
            if let Some(payload_type) = payloads
                .iter()
                .find(|payload| mappings.get(payload) == Some(codec))
            {
                return Ok(Self {
                    remote,
                    codec: *codec,
                    payload_type: *payload_type,
                    dtmf_payload: dtmf.filter(|payload| payloads.contains(payload)),
                    direction,
                });
            }
        }
        Err(SipError::Unsupported)
    }

    pub fn answer(&self, local: SocketAddr, session_id: u64) -> String {
        let family = if local.is_ipv4() { "IP4" } else { "IP6" };
        let format = match self.codec {
            Codec::Pcmu => "PCMU/8000",
            Codec::Pcma => "PCMA/8000",
            Codec::Opus => "opus/48000/2",
        };
        let mut output = format!("v=0\r\no=rusterisk {session_id} {session_id} IN {family} {}\r\ns=Rusterisk\r\nc=IN {family} {}\r\nt=0 0\r\nm=audio {} RTP/AVP {}", local.ip(), local.ip(), local.port(), self.payload_type);
        if let Some(payload) = self.dtmf_payload {
            output.push_str(&format!(" {payload}"));
        }
        output.push_str(&format!("\r\na=rtpmap:{} {format}\r\n", self.payload_type));
        if let Some(payload) = self.dtmf_payload {
            output.push_str(&format!(
                "a=rtpmap:{payload} telephone-event/8000\r\na=fmtp:{payload} 0-16\r\n"
            ));
        }
        output.push_str(match self.direction {
            Direction::SendRecv => "a=sendrecv\r\n",
            Direction::SendOnly => "a=recvonly\r\n",
            Direction::RecvOnly => "a=sendonly\r\n",
            Direction::Inactive => "a=inactive\r\n",
        });
        output
    }
}

pub enum SipEvent {
    Ringing {
        id: String,
        generation: u64,
    },
    Answered {
        id: String,
        generation: u64,
        session: Session,
    },
    Ended {
        id: String,
        generation: u64,
        cause: u16,
    },
    Incoming {
        id: String,
        endpoint: String,
        identity: String,
        caller: String,
        context: String,
        extension: String,
        session: Session,
    },
    IncomingEnded {
        id: String,
        cause: u16,
    },
}
pub struct Datagram {
    pub peer: SocketAddr,
    pub bytes: Vec<u8>,
}
pub struct Output {
    pub datagrams: Vec<Datagram>,
    pub events: Vec<SipEvent>,
}

enum DialogReply {
    Accepted {
        bytes: Vec<u8>,
        events: Vec<SipEvent>,
    },
    Rejected(Vec<u8>),
}

impl Output {
    fn response(peer: SocketAddr, bytes: Vec<u8>) -> Self {
        Self {
            datagrams: [Datagram { peer, bytes }].into_iter().collect(),
            events: Vec::new(),
        }
    }
}

struct Nonce {
    peer: SocketAddr,
    call_id: String,
    expires: u64,
    count: u32,
}
struct Cached {
    request: Request,
    peer: SocketAddr,
    bytes: Vec<u8>,
    expires: u64,
    retry: Option<ServerRetry>,
}

struct ServerRetry {
    next: u64,
    interval: u64,
    tag: String,
}

impl ServerRetry {
    fn for_reply(request: &Request, bytes: &[u8], now: u64) -> Result<Option<Self>, SipError> {
        if request.method != Method::Invite {
            return Ok(None);
        }
        let response = match SipMessage::try_from(bytes)? {
            SipMessage::Response(response) => response,
            SipMessage::Request(_) => return Err(SipError::InvalidPacket),
        };
        if response.status_code.code() < 300 {
            return Ok(None);
        }
        let to = response.to_header()?.typed()?;
        let tag = to.tag().ok_or(SipError::InvalidPacket)?.value().to_owned();
        Ok(Some(Self {
            next: now.saturating_add(500),
            interval: 500,
            tag,
        }))
    }
}
pub struct PendingInvite {
    pub id: String,
    pub peer: SocketAddr,
    pub request: Request,
    pub session: Session,
    pub local_tag: String,
    local: SocketAddr,
}

pub struct Engine {
    config: Config,
    seed: [u8; 32],
    serial: u64,
    nonces: BTreeMap<String, Nonce>,
    cache: BTreeMap<String, Cached>,
    pending: BTreeMap<String, PendingInvite>,
    contacts: BTreeMap<String, (SocketAddr, u64)>,
    outgoing: BTreeMap<String, outgoing::Outgoing>,
    incoming: BTreeMap<String, incoming::Incoming>,
    transactions: transactions::Transactions,
}

impl Engine {
    pub fn new(config: Config, seed: [u8; 32]) -> Self {
        Self {
            config,
            seed,
            serial: 0,
            nonces: BTreeMap::new(),
            cache: BTreeMap::new(),
            pending: BTreeMap::new(),
            contacts: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
            transactions: transactions::Transactions::default(),
        }
    }
    pub fn pending(&self, id: &str) -> Option<&PendingInvite> {
        self.pending.values().find(|invite| invite.id == id)
    }

    fn token(&mut self) -> Result<String, SipError> {
        self.serial = self.serial.checked_add(1).ok_or(SipError::Capacity)?;
        let mut hash = Sha256::new();
        hash.update(self.seed);
        hash.update(self.serial.to_be_bytes());
        let digest = hash.finalize();
        let alphabet = b"0123456789abcdef";
        let mut encoded = [0u8; 64];
        for (pair, byte) in encoded.as_chunks_mut::<2>().0.iter_mut().zip(digest) {
            pair[0] = alphabet[usize::from(byte >> 4)];
            pair[1] = alphabet[usize::from(byte & 15)];
        }
        String::from_utf8(encoded.to_vec()).map_err(|_| SipError::InvalidPacket)
    }

    fn authorize(
        &mut self,
        request: &Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Option<usize>, SipError> {
        let Some(header) = request.authorization_header() else {
            return Ok(None);
        };
        let authorization = match Authorization::parse(header.value()) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let Some(nonce) = self.nonces.get(&authorization.nonce) else {
            return Ok(None);
        };
        if nonce.peer != peer
            || nonce.expires <= now
            || nonce.call_id != request.call_id_header()?.value()
            || authorization.uri != request.uri
            || authorization.algorithm.unwrap_or(Algorithm::Md5) != Algorithm::Md5
        {
            return Ok(None);
        }
        let count = match authorization.qop {
            Some(AuthQop::Auth { nc, .. }) => nc,
            Some(AuthQop::AuthInt { .. }) => return Ok(None),
            None => 1,
        };
        if count == 0 || count <= nonce.count {
            return Ok(None);
        }
        let Some((endpoint_index, auth)) =
            self.config
                .endpoints
                .iter()
                .enumerate()
                .find_map(|(index, endpoint)| {
                    self.config
                        .auth
                        .iter()
                        .find(|auth| {
                            auth.name == endpoint.auth
                                && auth.username == authorization.username
                                && auth.realm == authorization.realm
                        })
                        .map(|auth| (index, auth))
                })
        else {
            return Ok(None);
        };
        let digest = DigestGenerator::from(&authorization, auth.password.expose(), &request.method)
            .compute();
        if !bool::from(digest.as_bytes().ct_eq(authorization.response.as_bytes())) {
            return Ok(None);
        }
        let endpoint = &self.config.endpoints[endpoint_index];
        if self
            .config
            .identify
            .iter()
            .any(|entry| entry.endpoint == endpoint.name)
            && !self
                .config
                .identify
                .iter()
                .any(|entry| entry.endpoint == endpoint.name && entry.address == peer.ip())
        {
            return Ok(None);
        }
        let nonce = self
            .nonces
            .get_mut(&authorization.nonce)
            .ok_or(SipError::InvalidPacket)?;
        nonce.count = count;
        Ok(Some(endpoint_index))
    }

    fn challenge(
        &mut self,
        request: &Request,
        peer: SocketAddr,
        now: u64,
    ) -> Result<Vec<u8>, SipError> {
        if self.nonces.len() >= 128 {
            return response(request, 503, "Service Unavailable", "", "");
        }
        let nonce = self.token()?;
        let realm = self
            .config
            .auth
            .first()
            .map(|auth| auth.realm.as_str())
            .unwrap_or("rusterisk");
        let extra = format!("WWW-Authenticate: Digest realm=\"{realm}\", nonce=\"{nonce}\", algorithm=MD5, qop=\"auth\"\r\n");
        self.nonces.insert(
            nonce,
            Nonce {
                peer,
                call_id: request.call_id_header()?.value().into(),
                expires: now.saturating_add(60_000),
                count: 0,
            },
        );
        response(request, 401, "Unauthorized", &extra, "")
    }

    pub fn receive(
        &mut self,
        bytes: &[u8],
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        let output = self.receive_inner(bytes, peer, now)?;
        self.track_output(&output, now)?;
        Ok(output)
    }

    fn track_output(&mut self, output: &Output, now: u64) -> Result<(), SipError> {
        for datagram in &output.datagrams {
            match SipMessage::try_from(datagram.bytes.as_slice())? {
                SipMessage::Request(request) => {
                    if !matches!(request.method, Method::Invite | Method::Ack) {
                        self.transactions
                            .track(request, datagram.peer, &datagram.bytes, now)?;
                    }
                }
                SipMessage::Response(_) => {}
            }
        }
        Ok(())
    }

    fn receive_inner(
        &mut self,
        bytes: &[u8],
        peer: SocketAddr,
        now: u64,
    ) -> Result<Output, SipError> {
        if bytes.len() > MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        self.nonces.retain(|_, value| value.expires > now);
        self.cache.retain(|_, value| value.expires > now);
        self.contacts.retain(|_, (_, expires)| *expires > now);
        let request = match SipMessage::try_from(bytes)? {
            SipMessage::Request(request) => request,
            SipMessage::Response(response) => {
                if self.transactions.receive(&response, peer, now)? {
                    return Ok(Output {
                        datagrams: Vec::new(),
                        events: Vec::new(),
                    });
                }
                return self.receive_response(response, peer, now);
            }
        };
        if request.headers.iter().count() > 64
            || request.cseq_header()?.typed()?.method != request.method
            || request
                .headers
                .iter()
                .any(|header| header.value().contains(['\r', '\n']))
        {
            return Err(SipError::InvalidPacket);
        }
        if request.method == Method::Ack {
            self.incoming_ack(&request, peer)?;
            return Ok(Output {
                datagrams: Vec::new(),
                events: Vec::new(),
            });
        }
        let key = format!(
            "{peer}:{}:{}:{}",
            request.call_id_header()?.value(),
            request.cseq_header()?.value(),
            request.top_via_header()?.value()
        );
        if let Some(cached) = self.cache.get(&key) {
            if cached.request == request && cached.peer == peer {
                return Ok(Output::response(peer, cached.bytes.clone()));
            }
            return Ok(Output::response(
                peer,
                response(&request, 481, "Call/Transaction Does Not Exist", "", "")?,
            ));
        }
        if self.cache.len() >= 256 {
            return Ok(Output::response(
                peer,
                response(&request, 503, "Service Unavailable", "", "")?,
            ));
        }
        if request.method == Method::Cancel {
            return self.incoming_cancel(key, request, peer, now);
        }
        if let Some(reply) = self.outgoing_bye(&request, peer, now)? {
            match reply {
                DialogReply::Accepted { bytes, events } => {
                    return self.cache_reply(key, request, peer, now, bytes, events);
                }
                DialogReply::Rejected(bytes) => return Ok(Output::response(peer, bytes)),
            }
        }
        if let Some(reply) = self.incoming_request(&request, peer, now)? {
            match reply {
                DialogReply::Accepted { bytes, events } => {
                    return self.cache_reply(key, request, peer, now, bytes, events);
                }
                DialogReply::Rejected(bytes) => return Ok(Output::response(peer, bytes)),
            }
        }
        let mut events = Vec::new();
        let reply = match request.method {
            Method::Options => response(
                &request,
                200,
                "OK",
                "Allow: INVITE, ACK, CANCEL, BYE, OPTIONS, REGISTER, INFO, REFER\r\n",
                "",
            )?,
            Method::Invite | Method::Register => {
                if let Some(index) = self.authorize(&request, peer, now)? {
                    let endpoint = self.config.endpoints[index].clone();
                    if request.method == Method::Register {
                        let to = request.to_header()?.typed()?;
                        let username = to
                            .uri
                            .auth
                            .as_ref()
                            .map(|auth| auth.user.as_str())
                            .ok_or(SipError::InvalidPacket)?;
                        let expected = self
                            .config
                            .auth
                            .iter()
                            .find(|auth| auth.name == endpoint.auth)
                            .ok_or(SipError::InvalidPacket)?;
                        if username != expected.username {
                            response(&request, 403, "Forbidden", "", "")?
                        } else {
                            let expires = request
                                .expires_header()
                                .map(|value| value.value().parse::<u32>())
                                .transpose()
                                .map_err(|_| SipError::InvalidPacket)?
                                .unwrap_or(300)
                                .min(3600);
                            if expires == 0 {
                                self.contacts.remove(&endpoint.name);
                            } else {
                                self.contacts.insert(
                                    endpoint.name.clone(),
                                    (peer, now.saturating_add(u64::from(expires) * 1000)),
                                );
                            }
                            let extra = format!(
                                "Contact: {}\r\nExpires: {expires}\r\n",
                                request.contact_header()?.value()
                            );
                            response(&request, 200, "OK", &extra, "")?
                        }
                    } else if self.pending.len() + self.incoming.len() >= 64 {
                        response(&request, 486, "Busy Here", "", "")?
                    } else {
                        let session = match Session::parse(
                            std::str::from_utf8(&request.body)?,
                            &endpoint.codecs,
                        ) {
                            Ok(session) => session,
                            Err(SipError::Unsupported) => {
                                return Ok(Output::response(
                                    peer,
                                    response(&request, 488, "Not Acceptable Here", "", "")?,
                                ))
                            }
                            Err(error) => return Err(error),
                        };
                        let id = format!("sip-{}", self.token()?);
                        let from = request.from_header()?.typed()?;
                        let caller = from.uri.auth.map(|auth| auth.user).unwrap_or_default();
                        let extension = request
                            .uri
                            .auth
                            .as_ref()
                            .map(|auth| auth.user.clone())
                            .ok_or(SipError::InvalidPacket)?;
                        let identity = self
                            .config
                            .auth
                            .iter()
                            .find(|auth| auth.name == endpoint.auth)
                            .ok_or(SipError::InvalidPacket)?
                            .username
                            .clone();
                        let local_tag = self.token()?;
                        let local = self
                            .config
                            .transports
                            .iter()
                            .find(|transport| transport.name == endpoint.transport)
                            .ok_or(SipError::InvalidPacket)?
                            .bind;
                        events.push(SipEvent::Incoming {
                            id: id.clone(),
                            endpoint: endpoint.name,
                            identity,
                            caller,
                            context: endpoint.context,
                            extension,
                            session: session.clone(),
                        });
                        self.pending.insert(
                            request.call_id_header()?.value().into(),
                            PendingInvite {
                                id,
                                peer,
                                request: request.clone(),
                                session,
                                local_tag,
                                local,
                            },
                        );
                        response(&request, 100, "Trying", "", "")?
                    }
                } else {
                    self.challenge(&request, peer, now)?
                }
            }
            Method::Bye | Method::Cancel => {
                response(&request, 481, "Call/Transaction Does Not Exist", "", "")?
            }
            Method::Ack
            | Method::Info
            | Method::Message
            | Method::Notify
            | Method::PRack
            | Method::Publish
            | Method::Refer
            | Method::Subscribe
            | Method::Update
            | Method::Other(_) => response(&request, 501, "Not Implemented", "", "")?,
        };
        self.cache_reply(key, request, peer, now, reply, events)
    }

    fn cache_reply(
        &mut self,
        key: String,
        request: Request,
        peer: SocketAddr,
        now: u64,
        reply: Vec<u8>,
        events: Vec<SipEvent>,
    ) -> Result<Output, SipError> {
        let retry = ServerRetry::for_reply(&request, &reply, now)?;
        self.cache.insert(
            key,
            Cached {
                request,
                peer,
                bytes: reply.clone(),
                expires: now.saturating_add(32_000),
                retry,
            },
        );
        Ok(Output {
            datagrams: [Datagram { peer, bytes: reply }].into_iter().collect(),
            events,
        })
    }
}

fn response(
    request: &Request,
    code: u16,
    phrase: &str,
    extra: &str,
    body: &str,
) -> Result<Vec<u8>, SipError> {
    let mut to = request.to_header()?.typed()?;
    if code != 100 && to.tag().is_none() {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| SipError::Entropy)?;
        let alphabet = b"0123456789abcdef";
        let tag: String = bytes
            .iter()
            .flat_map(|byte| {
                [
                    alphabet[usize::from(byte >> 4)] as char,
                    alphabet[usize::from(byte & 15)] as char,
                ]
            })
            .collect();
        to = to.with_tag(tag.into());
    }
    let mut output = format!("SIP/2.0 {code} {phrase}\r\n");
    for via in request
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("Via"))
    {
        output.push_str(&format!("Via: {}\r\n", via.value()));
    }
    output.push_str(&format!(
        "From: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nServer: Rusterisk\r\n{extra}",
        request.from_header()?.value(),
        to,
        request.call_id_header()?.value(),
        request.cseq_header()?.value()
    ));
    if !body.is_empty() {
        output.push_str("Content-Type: application/sdp\r\n");
    }
    output.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    if output.len() > MAX_MESSAGE {
        return Err(SipError::Capacity);
    }
    Ok(output.into_bytes())
}
