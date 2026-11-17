#![cfg(feature = "signaling")]

use rusterisk_config::Config;
use rusterisk_input_fuzz::{registration_responses, OracleError, Seed, Target, MAX_INPUT};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationRetirement, RegistrationRetirementReceipt,
    RegistrationRetirementStatus, RegistrationTransaction, RegistrationTransactionEvent, SipError,
    MAX_MESSAGE,
};
use std::net::SocketAddr;

const UDP: &[u8] = b"SIP/2.0 200 Response\r\nVia: SIP/2.0/UDP 127.0.0.1:5068;rport;branch=z9hG4bK-reg-00000000000000000000000000005678-7\r\nFrom: <sip:account@provider.example>;tag=reg-00000000000000000000000000005678\r\nTo: <sip:account@provider.example>;tag=registrar\r\nCall-ID: reg-00000000000000000000000000001234@rusterisk\r\nCSeq: 7 REGISTER\r\nContent-Length: 0\r\n\r\n";
const TCP: &[u8] = b"SIP/2.0 200 Response\r\nVia: SIP/2.0/TCP 127.0.0.1:5068;rport;branch=z9hG4bK-reg-00000000000000000000000000005678-7\r\nFrom: <sip:account@provider.example>;tag=reg-00000000000000000000000000005678\r\nTo: <sip:account@provider.example>;tag=registrar\r\nCall-ID: reg-00000000000000000000000000001234@rusterisk\r\nCSeq: 7 REGISTER\r\nContent-Length: 0\r\n\r\n";
const TLS: &[u8] = b"SIP/2.0 200 Response\r\nVia: SIP/2.0/TLS 127.0.0.1:5068;rport;branch=z9hG4bK-reg-00000000000000000000000000005678-7\r\nFrom: <sips:account@provider.example>;tag=reg-00000000000000000000000000005678\r\nTo: <sips:account@provider.example>;tag=registrar\r\nCall-ID: reg-00000000000000000000000000001234@rusterisk\r\nCSeq: 7 REGISTER\r\nContent-Length: 0\r\n\r\n";

const SEEDS: [Seed; 7] = [
    Seed {
        name: "registration-udp-final",
        bytes: UDP,
        valid: true,
    },
    Seed {
        name: "registration-tcp-final",
        bytes: TCP,
        valid: true,
    },
    Seed {
        name: "registration-tls-final",
        bytes: TLS,
        valid: true,
    },
    Seed {
        name: "registration-incomplete",
        bytes: b"SIP/2.0 200 Response\r\nVia: ",
        valid: false,
    },
    Seed {
        name: "registration-invalid-status",
        bytes: b"SIP/2.0 ??? Response\r\nContent-Length: 0\r\n\r\n",
        valid: false,
    },
    Seed {
        name: "registration-invalid-line",
        bytes: b"SIP/2.0 200 Response\nContent-Length: 0\n\n",
        valid: false,
    },
    Seed {
        name: "registration-message-capacity",
        bytes: &[b'A'; MAX_MESSAGE + 1],
        valid: false,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

fn error_class(error: SipError) -> ErrorClass {
    match error {
        SipError::InvalidPacket => ErrorClass::InvalidPacket,
        SipError::Capacity => ErrorClass::Capacity,
        SipError::Unsupported => ErrorClass::Unsupported,
        SipError::Entropy => ErrorClass::Entropy,
        SipError::Tag => ErrorClass::Tag,
        SipError::Config(_) => ErrorClass::Config,
        SipError::Parse(_) => ErrorClass::Parse,
        SipError::Address(_) => ErrorClass::Address,
        SipError::Utf8(_) => ErrorClass::Utf8,
        SipError::Io(error) => ErrorClass::Io(error.kind()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receipt {
    Unmatched,
    Provisional(u16),
    Final(u16),
    Error(ErrorClass),
    UnexpectedTransmit,
    UnexpectedTimeout,
}

fn live(result: Result<Option<RegistrationTransactionEvent>, SipError>) -> Receipt {
    match result {
        Ok(None) => Receipt::Unmatched,
        Ok(Some(RegistrationTransactionEvent::Provisional(status))) => Receipt::Provisional(status),
        Ok(Some(RegistrationTransactionEvent::Response(response))) => {
            Receipt::Final(response.status())
        }
        Ok(Some(RegistrationTransactionEvent::Transmit(_))) => Receipt::UnexpectedTransmit,
        Ok(Some(RegistrationTransactionEvent::TimedOut)) => Receipt::UnexpectedTimeout,
        Err(error) => Receipt::Error(error_class(error)),
    }
}

fn retired(result: Result<RegistrationRetirementReceipt, SipError>) -> Receipt {
    match result {
        Ok(RegistrationRetirementReceipt::Unmatched) => Receipt::Unmatched,
        Ok(RegistrationRetirementReceipt::Provisional(status)) => Receipt::Provisional(status),
        Ok(RegistrationRetirementReceipt::Final(status)) => Receipt::Final(status),
        Err(error) => Receipt::Error(error_class(error)),
    }
}

struct Pair {
    live: RegistrationTransaction,
    original: RegistrationTransaction,
    retirement: RegistrationRetirement,
    packet: Datagram,
}

fn transmitted(event: Option<RegistrationTransactionEvent>) -> Result<Datagram, SipError> {
    match event {
        Some(RegistrationTransactionEvent::Transmit(packet)) => Ok(packet),
        Some(
            RegistrationTransactionEvent::Provisional(_)
            | RegistrationTransactionEvent::Response(_)
            | RegistrationTransactionEvent::TimedOut,
        )
        | None => Err(SipError::InvalidPacket),
    }
}

impl Pair {
    fn new(protocol: &str) -> Result<Self, SipError> {
        let (server, client, tls) = match protocol {
            "udp" => (
                "sip:provider.example\\;transport=udp",
                "sip:account@provider.example",
                "",
            ),
            "tcp" => (
                "sip:provider.example\\;transport=tcp",
                "sip:account@provider.example",
                "",
            ),
            "tls" => (
                "sips:provider.example\\;transport=tcp",
                "sips:account@provider.example",
                "cert_file=fuzz-certificate.pem\npriv_key_file=fuzz-key.pem\n",
            ),
            _ => return Err(SipError::Unsupported),
        };
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[fuzzer]\ntype=user\npassword=fuzz-password\n",
            &format!("[stream]\ntype=transport\nprotocol={protocol}\nbind=127.0.0.1:5068\n{tls}[credential]\ntype=auth\nusername=account\npassword=sample\n[provider]\ntype=registration\ntransport=stream\noutbound_auth=credential\nserver_uri={server}\nclient_uri={client}\ncontact_user=line+1\n"),
            "",
            "",
        )?;
        let settings = config
            .registrations
            .first()
            .ok_or(SipError::InvalidPacket)?;
        let local = SocketAddr::from(([127, 0, 0, 1], 5068));
        let peer = SocketAddr::from(([127, 0, 0, 2], 5070));
        let identity = RegistrationIdentity::new(0x1234, 7, 0x5678)?;
        let mut live = RegistrationTransaction::new(settings, local, peer, identity, 1000)?;
        let packet = transmitted(live.poll(1000)?)?;
        let mut original = RegistrationTransaction::new(settings, local, peer, identity, 1000)?;
        let old = transmitted(original.poll(1000)?)?;
        assert!(
            old.bytes == packet.bytes,
            "Live and retired requests lost their shared fixture metadata"
        );
        let retirement = original
            .prepare_retirement(1000)?
            .ok_or(SipError::InvalidPacket)?;
        original.cancel();
        Ok(Self {
            live,
            original,
            retirement,
            packet,
        })
    }

    fn compare(&mut self, bytes: &[u8], peer: SocketAddr, now: u64) -> Receipt {
        let expected = live(self.live.receive(bytes, peer, now));
        let actual = retired(self.retirement.receive(bytes, peer, now));
        assert_eq!(
            actual, expected,
            "Live and retired REGISTER response classifications differ"
        );
        assert_eq!(self.live.expires_at(), 33_000);
        assert_eq!(self.original.expires_at(), 33_000);
        assert_eq!(self.retirement.expires_at(), 33_000);
        assert!(self
            .original
            .poll(u64::MAX)
            .is_ok_and(|event| event.is_none()));
        assert!(self
            .original
            .receive(bytes, peer, 0)
            .is_ok_and(|event| event.is_none()));
        actual
    }
}

fn canonical(protocol: &str) -> Result<&'static [u8], SipError> {
    match protocol {
        "udp" => Ok(UDP),
        "tcp" => Ok(TCP),
        "tls" => Ok(TLS),
        _ => Err(SipError::Unsupported),
    }
}

fn classified(bytes: &[u8]) -> Result<usize, SipError> {
    let mut matched = 0;
    for protocol in ["udp", "tcp", "tls"] {
        let mut pair = Pair::new(protocol)?;
        let next = pair.live.next_deadline();
        let receipt = pair.compare(bytes, pair.packet.peer, 1100);
        match receipt {
            Receipt::Provisional(_) | Receipt::Final(_) => matched += 1,
            Receipt::Unmatched | Receipt::Error(_) => {
                assert_eq!(pair.live.next_deadline(), next);
                assert!(pair.live.poll(1000)?.is_none());
                assert_eq!(
                    pair.retirement.poll(1000)?,
                    RegistrationRetirementStatus::Waiting
                );
                assert_eq!(
                    pair.compare(canonical(protocol)?, pair.packet.peer, 1100),
                    Receipt::Final(200)
                );
            }
            Receipt::UnexpectedTransmit | Receipt::UnexpectedTimeout => {
                return Err(SipError::InvalidPacket);
            }
        }
    }
    Ok(matched)
}

fn checked_zero(bytes: &[u8]) -> Result<(), SipError> {
    assert_eq!(
        classified(bytes)?,
        0,
        "A negative seed matched the controlled REGISTER identity"
    );
    assert_eq!(registration_responses(bytes), Ok(0));
    Ok(())
}

#[test]
fn fixed_registration_seeds_match_real_live_and_retired_transactions() -> Result<(), SipError> {
    for seed in &SEEDS {
        assert!(!seed.name.is_empty() && !seed.bytes.is_empty() && seed.bytes.len() <= MAX_INPUT);
        assert!(
            Target::Sip.seeds().iter().any(|maintained| {
                maintained.name == seed.name
                    && maintained.bytes == seed.bytes
                    && maintained.valid == seed.valid
            }),
            "{} is absent or changed in the maintained SIP corpus",
            seed.name
        );
        let count = classified(seed.bytes)?;
        let actual = registration_responses(seed.bytes);
        if seed.valid {
            assert!(
                count > 0,
                "{} did not reach the controlled REGISTER response",
                seed.name
            );
            assert!(
                matches!(actual, Ok(accepted) if accepted > 0),
                "{}: {actual:?}",
                seed.name
            );
        } else {
            assert_eq!(count, 0, "{} matched unexpectedly", seed.name);
            assert_eq!(
                actual,
                Ok(0),
                "{}: a shared checked rejection became an oracle failure",
                seed.name
            );
        }
    }
    Ok(())
}

#[test]
fn provisional_and_final_statuses_reach_the_same_checked_receipt_family() -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let base = std::str::from_utf8(canonical(protocol)?)?;
        for status in [100, 180, 200, 299, 401, 407, 423, 503, 603, 699] {
            let bytes = base.replacen(
                "SIP/2.0 200 Response",
                &format!("SIP/2.0 {status} Response"),
                1,
            );
            let mut pair = Pair::new(protocol)?;
            let wanted = if status < 200 {
                Receipt::Provisional(status)
            } else {
                Receipt::Final(status)
            };
            assert_eq!(
                pair.compare(bytes.as_bytes(), pair.packet.peer, 1100),
                wanted
            );
            assert!(matches!(registration_responses(bytes.as_bytes()), Ok(count) if count > 0));
        }
    }
    Ok(())
}

#[test]
fn unrelated_identity_and_request_bytes_cannot_count_as_registered_responses(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let source = std::str::from_utf8(canonical(protocol)?)?;
        for changed in [
            source.replace("branch=z9hG4bK", "branch=z9hG4bK-other"),
            source.replace("127.0.0.1:5068", "127.0.0.1:5069"),
            source.replace("Call-ID: reg-", "Call-ID: other-"),
            source.replace("CSeq: 7 REGISTER", "CSeq: 8 REGISTER"),
            source.replace("CSeq: 7 REGISTER", "CSeq: 7 INVITE"),
            source.replacen(";tag=reg-", ";tag=other-", 1),
        ] {
            assert!(
                changed != source,
                "A negative seed mutation left its wire unchanged"
            );
            checked_zero(changed.as_bytes())?;
        }
        checked_zero(&Pair::new(protocol)?.packet.bytes)?;
        let mut pair = Pair::new(protocol)?;
        assert_eq!(
            pair.compare(
                canonical(protocol)?,
                SocketAddr::from(([127, 0, 0, 3], 5070)),
                1100
            ),
            Receipt::Unmatched
        );
    }
    Ok(())
}

#[test]
fn parser_framing_and_capacity_errors_remain_distinct_checked_rejections() -> Result<(), SipError> {
    let parse = b"SIP/2.0 ??? Response\r\nContent-Length: 0\r\n\r\n";
    let capacity = vec![b'A'; MAX_MESSAGE + 1];
    for protocol in ["udp", "tcp", "tls"] {
        let source = std::str::from_utf8(canonical(protocol)?)?;
        for changed in [
            source.replace("Content-Length: 0", "Content-Length: 1"),
            source.replace("Content-Length: 0", "Content-Length: 0\r\nl: 0"),
            source.replace("\r\n", "\n"),
            format!("{source}trailing"),
            source.replace("CSeq: 7 REGISTER", "CSeq: 7 REGISTER\r\nCSeq: 7 REGISTER"),
            source.replace(";branch=", ";branch=z9hG4bK-other;branch="),
        ] {
            let mut pair = Pair::new(protocol)?;
            assert_eq!(
                pair.compare(changed.as_bytes(), pair.packet.peer, 1100),
                Receipt::Error(ErrorClass::InvalidPacket)
            );
            checked_zero(changed.as_bytes())?;
        }
        for (bytes, class) in [
            (parse.as_slice(), ErrorClass::Parse),
            (capacity.as_slice(), ErrorClass::Capacity),
        ] {
            let mut pair = Pair::new(protocol)?;
            assert_eq!(
                pair.compare(bytes, pair.packet.peer, 1100),
                Receipt::Error(class)
            );
            checked_zero(bytes)?;
        }
    }
    checked_zero(b"")?;
    checked_zero(b"\xff\0")?;
    Ok(())
}

#[test]
fn final_and_deadline_settlement_cannot_revive_a_retired_request() -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let mut final_pair = Pair::new(protocol)?;
        assert_eq!(
            final_pair.compare(canonical(protocol)?, final_pair.packet.peer, 1100),
            Receipt::Final(200)
        );
        for now in [1100, 33_000, u64::MAX, 0] {
            assert_eq!(
                final_pair.retirement.poll(now)?,
                RegistrationRetirementStatus::Released
            );
            assert_eq!(
                final_pair
                    .retirement
                    .receive(b"malformed", final_pair.packet.peer, now)?,
                RegistrationRetirementReceipt::Unmatched
            );
        }
        let mut timed = Pair::new(protocol)?;
        assert_eq!(
            timed.retirement.poll(32_999)?,
            RegistrationRetirementStatus::Waiting
        );
        assert_eq!(
            timed
                .retirement
                .receive(canonical(protocol)?, timed.packet.peer, 33_000)?,
            RegistrationRetirementReceipt::Unmatched
        );
        for now in [33_000, u64::MAX, 0] {
            assert_eq!(
                timed.retirement.poll(now)?,
                RegistrationRetirementStatus::Released
            );
            assert_eq!(timed.retirement.expires_at(), 33_000);
        }
        assert!(matches!(registration_responses(canonical(protocol)?), Ok(count) if count > 0));
    }
    Ok(())
}

#[test]
fn backward_clocks_preserve_the_prior_provisional_receipt_and_original_deadline(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let mut pair = Pair::new(protocol)?;
        let response = std::str::from_utf8(canonical(protocol)?)?;
        let provisional = response.replacen("SIP/2.0 200 Response", "SIP/2.0 100 Response", 1);
        assert_eq!(
            pair.compare(provisional.as_bytes(), pair.packet.peer, 1100),
            Receipt::Provisional(100)
        );
        assert_eq!(
            pair.compare(canonical(protocol)?, pair.packet.peer, 1099),
            Receipt::Error(ErrorClass::InvalidPacket)
        );
        assert_eq!(
            pair.retirement.poll(1100)?,
            RegistrationRetirementStatus::Waiting
        );
        assert_eq!(
            pair.compare(canonical(protocol)?, pair.packet.peer, 1100),
            Receipt::Final(200)
        );
        assert!(matches!(registration_responses(provisional.as_bytes()), Ok(count) if count > 0));
    }
    Ok(())
}

#[test]
fn input_capacity_is_checked_without_adding_another_fuzz_target() -> Result<(), SipError> {
    assert_eq!(
        Target::ALL,
        [
            Target::Settings,
            Target::Controls,
            Target::Sip,
            Target::Media
        ]
    );
    checked_zero(&vec![b'A'; MAX_INPUT])?;
    assert_eq!(
        registration_responses(&vec![b'A'; MAX_INPUT + 1]),
        Err(OracleError::InputCapacity)
    );
    Ok(())
}
