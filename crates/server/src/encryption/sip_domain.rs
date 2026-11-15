use hickory_resolver::proto::rr::Name;
use rsipstack::sip::{uri::Host, Uri};
use tokio_rustls::rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        verify_server_cert_signed_by_trust_anchor, verify_server_name,
    },
    crypto::{aws_lc_rs, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
    CertificateError, ClientConfig, DigitallySignedStruct, Error, SignatureScheme,
};
use x509_parser::extensions::GeneralName;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(crate) struct SipDomainVerifier {
    roots: ::tokio_rustls::rustls::RootCertStore,
}

pub(super) fn sip_client_configuration(
    roots: ::tokio_rustls::rustls::RootCertStore,
) -> ClientConfig {
    let verifier = SipDomainVerifier { roots };
    #[expect(
        clippy::disallowed_types,
        reason = "docs/adr/verify-sip-domains-with-an-immutable-library-callback.md: Share only immutable SIP certificate trust through the fixed verifier API."
    )]
    let verifier =
        ::std::sync::Arc::<crate::encryption::sip_domain::SipDomainVerifier>::new(verifier);
    ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth()
}

fn encoding() -> Error {
    Error::InvalidCertificate(CertificateError::BadEncoding)
}
fn identity() -> Error {
    Error::InvalidCertificate(CertificateError::NotValidForName)
}

fn canonical_name(value: &str) -> Result<Option<String>, Error> {
    if value.len() > 1024 {
        return Err(encoding());
    }
    if value.starts_with('.') || value.contains('*') {
        return Ok(None);
    }
    let name = Name::from_utf8(value).map_err(|_| encoding())?;
    if name.is_root() {
        return Err(encoding());
    }
    Ok(Some(
        name.to_ascii().trim_end_matches('.').to_ascii_lowercase(),
    ))
}

fn sip_host(value: &str) -> Result<Option<String>, Error> {
    if value.len() > 2048 {
        return Err(encoding());
    }
    let Some((scheme, _)) = value.split_once(':') else {
        return Ok(None);
    };
    if !scheme.eq_ignore_ascii_case("sip") {
        return Ok(None);
    }
    let uri = Uri::try_from(value).map_err(|_| encoding())?;
    if uri.auth.is_some() {
        return Ok(None);
    }
    Ok(Some(match uri.host_with_port.host {
        Host::Domain(name) => name.0,
        Host::IpAddr(address) => address.to_string(),
    }))
}

fn verify_identity(certificate: &CertificateDer<'_>, expected: &str) -> Result<(), Error> {
    let expected = canonical_name(expected)?.ok_or_else(identity)?;
    let (remaining, parsed) =
        x509_parser::parse_x509_certificate(certificate.as_ref()).map_err(|_| encoding())?;
    if !remaining.is_empty() {
        return Err(encoding());
    }
    let names = parsed
        .subject_alternative_name()
        .map_err(|_| encoding())?
        .ok_or_else(identity)?;
    let names = &names.value.general_names;
    if names.len() > 128 {
        return Err(encoding());
    }
    let mut sip_identity = false;
    for name in names {
        if let GeneralName::URI(value) = name {
            if let Some(host) = sip_host(value)? {
                sip_identity = true;
                if canonical_name(&host)?.is_some_and(|name| name == expected) {
                    return Ok(());
                }
            }
        }
    }
    if sip_identity {
        return Err(identity());
    }
    for name in names {
        if let GeneralName::DNSName(value) = name {
            if canonical_name(value)?.is_some_and(|name| name == expected) {
                return Ok(());
            }
        }
    }
    Err(identity())
}

impl ServerCertVerifier for SipDomainVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if end_entity.len() > 65536
            || intermediates.len() > 7
            || intermediates
                .iter()
                .any(|certificate| certificate.len() > 65536)
        {
            return Err(encoding());
        }
        let parsed = ParsedCertificate::try_from(end_entity)?;
        verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &self.roots,
            intermediates,
            now,
            aws_lc_rs::default_provider()
                .signature_verification_algorithms
                .all,
        )?;
        match server_name {
            ServerName::IpAddress(_) => verify_server_name(&parsed, server_name)?,
            ServerName::DnsName(name) => verify_identity(end_entity, name.as_ref())?,
            _ => return Err(identity()),
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            certificate,
            signed,
            &aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            certificate,
            signed,
            &aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
