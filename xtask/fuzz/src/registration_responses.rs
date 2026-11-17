use crate::{
    targets::{REGISTRATION_TCP, REGISTRATION_TLS, REGISTRATION_UDP},
    OracleError, MAX_INPUT,
};
use rusterisk_config::{Config, Registration};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationRetirement, RegistrationRetirementReceipt,
    RegistrationRetirementStatus, RegistrationTransaction, RegistrationTransactionEvent, SipError,
    MAX_MESSAGE,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const BEGAN: u64 = 1000;
const ARRIVED: u64 = 1100;
const EXPIRES: u64 = 33_000;
const LOCAL: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5068);
const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), 5070);
const FOREIGN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 3)), 5070);

#[derive(Clone, Copy)]
enum Transport {
    Udp,
    Tcp,
    Tls,
}

impl Transport {
    fn configuration(self) -> Result<Config, OracleError> {
        let (name, server, client, tls) = match self {
            Self::Udp => (
                "udp",
                "sip:provider.example\\;transport=udp",
                "sip:account@provider.example",
                "",
            ),
            Self::Tcp => (
                "tcp",
                "sip:provider.example\\;transport=tcp",
                "sip:account@provider.example",
                "",
            ),
            Self::Tls => (
                "tls",
                "sips:provider.example\\;transport=tcp",
                "sips:account@provider.example",
                "cert_file=fuzz-certificate.pem\npriv_key_file=fuzz-key.pem\n",
            ),
        };
        Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[fuzzer]\ntype=user\npassword=fuzz-password\n",
            &format!("[stream]\ntype=transport\nprotocol={name}\nbind=127.0.0.1:5068\n{tls}[credential]\ntype=auth\nusername=account\npassword=sample\n[provider]\ntype=registration\ntransport=stream\noutbound_auth=credential\nserver_uri={server}\nclient_uri={client}\ncontact_user=line+1\n"),
            "",
            "",
        )
        .map_err(|_| OracleError::RegistrationSetup)
    }

    const fn canonical(self) -> &'static [u8] {
        match self {
            Self::Udp => REGISTRATION_UDP,
            Self::Tcp => REGISTRATION_TCP,
            Self::Tls => REGISTRATION_TLS,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ErrorClass {
    InvalidPacket,
    Capacity,
    Unsupported,
    Entropy,
    Tag,
    Config,
    Parse,
    Address,
    Utf8,
    Io(std::io::ErrorKind),
}

impl From<SipError> for ErrorClass {
    fn from(error: SipError) -> Self {
        match error {
            SipError::InvalidPacket => Self::InvalidPacket,
            SipError::Capacity => Self::Capacity,
            SipError::Unsupported => Self::Unsupported,
            SipError::Entropy => Self::Entropy,
            SipError::Tag => Self::Tag,
            SipError::Config(_) => Self::Config,
            SipError::Parse(_) => Self::Parse,
            SipError::Address(_) => Self::Address,
            SipError::Utf8(_) => Self::Utf8,
            SipError::Io(error) => Self::Io(error.kind()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Receipt {
    Unmatched,
    Provisional(u16),
    Final(u16),
    Error(ErrorClass),
}

fn live(
    result: Result<Option<RegistrationTransactionEvent>, SipError>,
) -> Result<Receipt, OracleError> {
    match result {
        Ok(None) => Ok(Receipt::Unmatched),
        Ok(Some(RegistrationTransactionEvent::Provisional(status))) => {
            Ok(Receipt::Provisional(status))
        }
        Ok(Some(RegistrationTransactionEvent::Response(response))) => {
            Ok(Receipt::Final(response.status()))
        }
        Ok(Some(
            RegistrationTransactionEvent::Transmit(_) | RegistrationTransactionEvent::TimedOut,
        )) => Err(OracleError::RegistrationReceipt),
        Err(error) => Ok(Receipt::Error(error.into())),
    }
}

fn retired(result: Result<RegistrationRetirementReceipt, SipError>) -> Receipt {
    match result {
        Ok(RegistrationRetirementReceipt::Unmatched) => Receipt::Unmatched,
        Ok(RegistrationRetirementReceipt::Provisional(status)) => Receipt::Provisional(status),
        Ok(RegistrationRetirementReceipt::Final(status)) => Receipt::Final(status),
        Err(error) => Receipt::Error(error.into()),
    }
}

fn transmitted(
    result: Result<Option<RegistrationTransactionEvent>, SipError>,
) -> Result<Datagram, OracleError> {
    match result {
        Ok(Some(RegistrationTransactionEvent::Transmit(packet))) => Ok(packet),
        Ok(Some(
            RegistrationTransactionEvent::Provisional(_)
            | RegistrationTransactionEvent::Response(_)
            | RegistrationTransactionEvent::TimedOut,
        ))
        | Ok(None)
        | Err(_) => Err(OracleError::RegistrationSetup),
    }
}

struct Pair {
    live: RegistrationTransaction,
    original: RegistrationTransaction,
    retirement: RegistrationRetirement,
}

impl Pair {
    fn new(settings: &Registration) -> Result<Self, OracleError> {
        let identity = RegistrationIdentity::new(0x1234, 7, 0x5678)
            .map_err(|_| OracleError::RegistrationSetup)?;
        let mut live = RegistrationTransaction::new(settings, LOCAL, PEER, identity, BEGAN)
            .map_err(|_| OracleError::RegistrationSetup)?;
        let packet = transmitted(live.poll(BEGAN))?;
        let mut original = RegistrationTransaction::new(settings, LOCAL, PEER, identity, BEGAN)
            .map_err(|_| OracleError::RegistrationSetup)?;
        let old = transmitted(original.poll(BEGAN))?;
        if old.peer != packet.peer
            || packet.peer != PEER
            || old.bytes != packet.bytes
            || packet.bytes.len() > MAX_MESSAGE
        {
            return Err(OracleError::RegistrationSetup);
        }
        let retirement = original
            .prepare_retirement(BEGAN)
            .map_err(|_| OracleError::RegistrationSetup)?
            .ok_or(OracleError::RegistrationSetup)?;
        original.cancel();
        let mut pair = Self {
            live,
            original,
            retirement,
        };
        pair.deadlines()?;
        pair.cancelled(&packet.bytes)?;
        Ok(pair)
    }

    fn deadlines(&self) -> Result<(), OracleError> {
        if self.live.expires_at() != EXPIRES
            || self.original.expires_at() != EXPIRES
            || self.retirement.expires_at() != EXPIRES
        {
            return Err(OracleError::RegistrationDeadline);
        }
        Ok(())
    }

    fn cancelled(&mut self, bytes: &[u8]) -> Result<(), OracleError> {
        if !self.original.is_finished()
            || self.original.next_deadline().is_some()
            || !matches!(self.original.poll(u64::MAX), Ok(None))
            || !matches!(self.original.receive(bytes, PEER, 0), Ok(None))
        {
            return Err(OracleError::RegistrationCancellation);
        }
        Ok(())
    }

    fn compare(
        &mut self,
        bytes: &[u8],
        peer: SocketAddr,
        now: u64,
    ) -> Result<Receipt, OracleError> {
        let expected = live(self.live.receive(bytes, peer, now))?;
        let actual = retired(self.retirement.receive(bytes, peer, now));
        if expected != actual {
            return Err(OracleError::RegistrationReceipt);
        }
        self.deadlines()?;
        self.cancelled(bytes)?;
        Ok(actual)
    }

    fn unchanged_clock(&mut self, next: Option<u64>) -> Result<(), OracleError> {
        if self.live.next_deadline() != next
            || !matches!(self.live.poll(BEGAN), Ok(None))
            || !matches!(
                self.retirement.poll(BEGAN),
                Ok(RegistrationRetirementStatus::Waiting)
            )
        {
            return Err(OracleError::RegistrationClock);
        }
        self.deadlines()
    }

    fn released(&mut self, bytes: &[u8]) -> Result<(), OracleError> {
        for now in [ARRIVED, EXPIRES, u64::MAX, 0] {
            if !matches!(
                self.retirement.poll(now),
                Ok(RegistrationRetirementStatus::Released)
            ) || self.compare(bytes, PEER, now)? != Receipt::Unmatched
                || self.compare(b"malformed", PEER, now)? != Receipt::Unmatched
            {
                return Err(OracleError::RegistrationSettlement);
            }
        }
        self.deadlines()
    }
}

fn check_input(pair: &mut Pair, bytes: &[u8], canonical: &[u8]) -> Result<usize, OracleError> {
    let next = pair.live.next_deadline();
    if pair.compare(canonical, FOREIGN, ARRIVED)? != Receipt::Unmatched {
        return Err(OracleError::RegistrationReceipt);
    }
    pair.unchanged_clock(next)?;
    let accepted = match pair.compare(bytes, PEER, ARRIVED)? {
        Receipt::Provisional(_) => {
            if pair.compare(canonical, PEER, ARRIVED - 1)?
                != Receipt::Error(ErrorClass::InvalidPacket)
            {
                return Err(OracleError::RegistrationClock);
            }
            if pair.compare(canonical, PEER, ARRIVED)? != Receipt::Final(200) {
                return Err(OracleError::RegistrationReceipt);
            }
            1
        }
        Receipt::Final(_) => 1,
        Receipt::Unmatched | Receipt::Error(_) => {
            pair.unchanged_clock(next)?;
            if pair.compare(canonical, PEER, ARRIVED)? != Receipt::Final(200) {
                return Err(OracleError::RegistrationReceipt);
            }
            0
        }
    };
    pair.released(bytes)?;
    Ok(accepted)
}

fn check_expiry(settings: &Registration, bytes: &[u8]) -> Result<(), OracleError> {
    let mut pair = Pair::new(settings)?;
    if !matches!(
        pair.retirement.poll(EXPIRES - 1),
        Ok(RegistrationRetirementStatus::Waiting)
    ) || pair.compare(bytes, PEER, EXPIRES)? != Receipt::Unmatched
        || !matches!(
            pair.live.poll(EXPIRES),
            Ok(Some(RegistrationTransactionEvent::TimedOut))
        )
    {
        return Err(OracleError::RegistrationDeadline);
    }
    pair.released(bytes)
}

pub fn registration_responses(bytes: &[u8]) -> Result<usize, OracleError> {
    if bytes.len() > MAX_INPUT {
        return Err(OracleError::InputCapacity);
    }
    let mut accepted = 0;
    for transport in [Transport::Udp, Transport::Tcp, Transport::Tls] {
        let configuration = transport.configuration()?;
        let settings = configuration
            .registrations
            .first()
            .ok_or(OracleError::RegistrationSetup)?;
        let mut pair = Pair::new(settings)?;
        accepted += check_input(&mut pair, bytes, transport.canonical())?;
        check_expiry(settings, bytes)?;
    }
    Ok(accepted)
}
