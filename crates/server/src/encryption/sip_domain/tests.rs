use super::SipDomainVerifier;
use crate::ControlError;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, CertifiedKey, ExtendedKeyUsagePurpose,
    IsCa, KeyPair, KeyUsagePurpose, SanType, SigningKey,
};
use std::time::Duration;
use tokio_rustls::rustls::{
    client::danger::{ServerCertVerified, ServerCertVerifier},
    internal::msgs::codec::Codec,
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, Error, RootCertStore, SignatureScheme,
};
mod handshakes;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn authority() -> Result<CertifiedIssuer<'static, KeyPair>, ControlError> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = [KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign]
        .into_iter()
        .collect();
    CertifiedIssuer::self_signed(params, KeyPair::generate().map_err(failure)?).map_err(failure)
}

fn parameters(names: impl IntoIterator<Item = SanType>) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.subject_alt_names = names.into_iter().collect();
    params.extended_key_usages = std::iter::once(ExtendedKeyUsagePurpose::ServerAuth).collect();
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2030, 1, 1);
    params
}

fn issue(
    authority: &CertifiedIssuer<'_, KeyPair>,
    params: &CertificateParams,
) -> Result<CertifiedKey<KeyPair>, ControlError> {
    let signing_key = KeyPair::generate().map_err(failure)?;
    let cert = params.signed_by(&signing_key, authority).map_err(failure)?;
    Ok(CertifiedKey { cert, signing_key })
}

fn verifier(authority: &CertificateDer<'_>) -> Result<SipDomainVerifier, ControlError> {
    let mut roots = RootCertStore::empty();
    roots.add(authority.clone().into_owned()).map_err(failure)?;
    Ok(SipDomainVerifier { roots })
}

fn verify(
    verifier: &SipDomainVerifier,
    certificate: &CertificateDer<'_>,
    name: &str,
) -> Result<ServerCertVerified, Error> {
    let name = ServerName::try_from(name)
        .map_err(|_| Error::General("Invalid fixture identity".into()))?;
    verifier.verify_server_cert(
        certificate,
        &[],
        &name,
        &[],
        UnixTime::since_unix_epoch(Duration::from_secs(1_800_000_000)),
    )
}

fn uri(value: &str) -> Result<SanType, ControlError> {
    Ok(SanType::URI(value.try_into().map_err(failure)?))
}
fn dns(value: &str) -> Result<SanType, ControlError> {
    Ok(SanType::DnsName(value.try_into().map_err(failure)?))
}

#[test]
fn sip_uri_identities_take_precedence_and_ignore_case_ports_and_parameters(
) -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let uri_only = issue(
        &authority,
        &parameters(std::iter::once(uri(
            "SiP:LOCALHOST:5070;transport=udp;user=phone",
        )?)),
    )?;
    assert!(verify(&verifier, uri_only.cert.der(), "localhost").is_ok());
    assert!(verify(&verifier, uri_only.cert.der(), "wrong.test").is_err());
    let precedence = issue(
        &authority,
        &parameters([uri("sip:other.test")?, dns("localhost")?]),
    )?;
    assert!(verify(&verifier, precedence.cert.der(), "localhost").is_err());
    for ignored in [
        "sip:user@other.test",
        "sips:other.test",
        "https://other.test",
    ] {
        let fallback = issue(&authority, &parameters([uri(ignored)?, dns("localhost")?]))?;
        assert!(
            verify(&verifier, fallback.cert.der(), "localhost").is_ok(),
            "{ignored}"
        );
    }
    Ok(())
}

#[test]
fn complete_dns_names_match_without_wildcard_or_suffix_expansion() -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    for (name, authenticated) in [
        ("phone.example.test", true),
        ("*.example.test", false),
        ("example.test", false),
        (".example.test", false),
    ] {
        let identity = issue(&authority, &parameters(std::iter::once(dns(name)?)))?;
        assert_eq!(
            verify(&verifier, identity.cert.der(), "phone.example.test").is_ok(),
            authenticated,
            "{name}"
        );
    }
    let exact = issue(
        &authority,
        &parameters([dns("*.example.test")?, dns("phone.example.test")?]),
    )?;
    assert!(verify(&verifier, exact.cert.der(), "phone.example.test").is_ok());
    let uri_wildcard = issue(
        &authority,
        &parameters([uri("sip:*.example.test")?, dns("phone.example.test")?]),
    )?;
    assert!(verify(&verifier, uri_wildcard.cert.der(), "phone.example.test").is_err());
    Ok(())
}

#[test]
fn certificate_paths_expiry_usage_names_and_signatures_fail_independently(
) -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let params = parameters(std::iter::once(dns("localhost")?));
    let identity = issue(&authority, &params)?;
    assert!(verify(&verifier, identity.cert.der(), "localhost").is_ok());
    assert!(verify(&verifier, identity.cert.der(), "wrong.test").is_err());
    let unrelated = self::authority()?;
    assert!(verify(
        &self::verifier(unrelated.der())?,
        identity.cert.der(),
        "localhost"
    )
    .is_err());
    let mut expired = params.clone();
    expired.not_before = rcgen::date_time_ymd(2020, 1, 1);
    expired.not_after = rcgen::date_time_ymd(2021, 1, 1);
    assert!(verify(
        &verifier,
        issue(&authority, &expired)?.cert.der(),
        "localhost"
    )
    .is_err());
    let mut future = params.clone();
    future.not_before = rcgen::date_time_ymd(2035, 1, 1);
    future.not_after = rcgen::date_time_ymd(2040, 1, 1);
    assert!(verify(
        &verifier,
        issue(&authority, &future)?.cert.der(),
        "localhost"
    )
    .is_err());
    let mut wrong_usage = params;
    wrong_usage.extended_key_usages =
        std::iter::once(ExtendedKeyUsagePurpose::ClientAuth).collect();
    assert!(verify(
        &verifier,
        issue(&authority, &wrong_usage)?.cert.der(),
        "localhost"
    )
    .is_err());
    let mut altered = identity.cert.der().to_vec();
    *altered.last_mut().ok_or(ControlError::Capacity)? ^= 1;
    assert!(verify(&verifier, &CertificateDer::from(altered), "localhost").is_err());
    Ok(())
}

#[test]
fn intermediate_certificates_complete_only_valid_trust_paths() -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let mut params = parameters(std::iter::empty());
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = [KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign]
        .into_iter()
        .collect();
    let intermediate =
        CertifiedIssuer::signed_by(params, KeyPair::generate().map_err(failure)?, &authority)
            .map_err(failure)?;
    let identity = issue(
        &intermediate,
        &parameters(std::iter::once(uri("sip:localhost")?)),
    )?;
    let name = ServerName::try_from("localhost").map_err(failure)?;
    let now = UnixTime::since_unix_epoch(Duration::from_secs(1_800_000_000));
    assert!(verifier
        .verify_server_cert(
            identity.cert.der(),
            std::slice::from_ref(intermediate.der()),
            &name,
            &[],
            now
        )
        .is_ok());
    assert!(verifier
        .verify_server_cert(identity.cert.der(), &[], &name, &[], now)
        .is_err());
    let mut altered = intermediate.der().to_vec();
    *altered.last_mut().ok_or(ControlError::Capacity)? ^= 1;
    assert!(verifier
        .verify_server_cert(
            identity.cert.der(),
            &[CertificateDer::from(altered)],
            &name,
            &[],
            now
        )
        .is_err());
    Ok(())
}

#[test]
fn numeric_targets_keep_library_address_verification() -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let address = "127.0.0.1".parse().map_err(failure)?;
    let identity = issue(
        &authority,
        &parameters(std::iter::once(SanType::IpAddress(address))),
    )?;
    assert!(verify(&verifier, identity.cert.der(), "127.0.0.1").is_ok());
    assert!(verify(&verifier, identity.cert.der(), "127.0.0.2").is_err());
    let uri_only = issue(
        &authority,
        &parameters(std::iter::once(uri("sip:127.0.0.1")?)),
    )?;
    assert!(verify(&verifier, uri_only.cert.der(), "127.0.0.1").is_err());
    Ok(())
}

#[test]
fn internationalized_names_use_the_same_complete_ascii_identity() -> Result<(), ControlError> {
    let unicode = super::canonical_name("bücher.example").map_err(failure)?;
    let ascii = super::canonical_name("xn--bcher-kva.example").map_err(failure)?;
    assert_eq!(unicode, ascii);
    assert_eq!(
        super::canonical_name("BÜCHER.Example.").map_err(failure)?,
        ascii
    );
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let identity = issue(
        &authority,
        &parameters(std::iter::once(uri("sip:xn--bcher-kva.example")?)),
    )?;
    assert!(verify(&verifier, identity.cert.der(), "xn--bcher-kva.example").is_ok());
    Ok(())
}

#[test]
fn common_names_without_subject_alternative_names_are_rejected() -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let mut params = parameters(std::iter::empty());
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let identity = issue(&authority, &params)?;
    assert!(verify(&verifier, identity.cert.der(), "localhost").is_err());
    Ok(())
}

#[test]
fn handshake_signatures_use_the_library_algorithms_for_both_protocol_versions(
) -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let identity = issue(&authority, &parameters(std::iter::once(dns("localhost")?)))?;
    let message = b"certificate verification fixture";
    let signature = identity.signing_key.sign(message).map_err(failure)?;
    let mut encoded = Vec::with_capacity(signature.len() + 4);
    SignatureScheme::ECDSA_NISTP256_SHA256.encode(&mut encoded);
    u16::try_from(signature.len())
        .map_err(failure)?
        .encode(&mut encoded);
    encoded.extend_from_slice(&signature);
    let signature = DigitallySignedStruct::read_bytes(&encoded)
        .map_err(|error| failure(format!("{error:?}")))?;
    assert!(verifier
        .verify_tls12_signature(message, identity.cert.der(), &signature)
        .is_ok());
    assert!(verifier
        .verify_tls13_signature(message, identity.cert.der(), &signature)
        .is_ok());
    assert!(verifier
        .verify_tls12_signature(b"altered", identity.cert.der(), &signature)
        .is_err());
    assert!(verifier
        .verify_tls13_signature(b"altered", identity.cert.der(), &signature)
        .is_err());
    Ok(())
}

#[test]
fn certificates_chains_and_identity_sets_are_bounded_before_authentication(
) -> Result<(), ControlError> {
    let authority = authority()?;
    let verifier = verifier(authority.der())?;
    let identity = issue(&authority, &parameters(std::iter::once(dns("localhost")?)))?;
    assert!(verify(
        &verifier,
        &CertificateDer::from(vec![0; 65537]),
        "localhost"
    )
    .is_err());
    let chain: [CertificateDer<'_>; 8] = std::array::from_fn(|_| identity.cert.der().clone());
    let name = ServerName::try_from("localhost").map_err(failure)?;
    assert!(verifier
        .verify_server_cert(
            identity.cert.der(),
            &chain,
            &name,
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(1_800_000_000))
        )
        .is_err());
    let names = std::iter::once(dns("localhost")?).chain(
        (0..128)
            .map(|_| dns("other.test"))
            .collect::<Result<Vec<_>, _>>()?,
    );
    assert!(verify(
        &verifier,
        issue(&authority, &parameters(names))?.cert.der(),
        "localhost"
    )
    .is_err());
    Ok(())
}
