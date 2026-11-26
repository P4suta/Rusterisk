use super::{failure, response};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Uri,
};
use rusterisk_server::ControlError;
use rusterisk_sip::{parse_registration_contacts, RegistrationContacts};
use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub(super) async fn register(
    socket: &UdpSocket,
    target: SocketAddr,
    username: &str,
    password: &str,
    serial: u32,
    expires: u32,
) -> Result<usize, ControlError> {
    let local = socket.local_addr().map_err(failure)?;
    let uri = Uri::try_from(format!("sip:{target}")).map_err(failure)?;
    let packet = |sequence: u32, authorization: &str| {
        format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-register-{serial}-{sequence};rport\r\nFrom: <sip:{username}@localhost>;tag=phone\r\nTo: <sip:{username}@localhost>\r\nCall-ID: registration-{serial}\r\nCSeq: {sequence} REGISTER\r\nContact: <sip:{username}@{local}>\r\nExpires: {expires}\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
    };
    socket
        .send_to(packet(1, "").as_bytes(), target)
        .await
        .map_err(failure)?;
    let challenge = response(socket).await?;
    assert_eq!(challenge.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(ControlError::Capacity)?
            .value(),
    )
    .map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "registration-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username,
        password,
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"{username}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"registration-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    socket
        .send_to(packet(2, &authorization).as_bytes(), target)
        .await
        .map_err(failure)?;
    let registered = response(socket).await?;
    assert_eq!(registered.status_code.code(), 200);
    let contacts = parse_registration_contacts(
        registered
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
            .map(|header| header.value()),
        None,
    )
    .map_err(failure)?;
    match contacts {
        RegistrationContacts::Query => Ok(0),
        RegistrationContacts::Bindings(contacts) => Ok(contacts.as_slice().len()),
        RegistrationContacts::RemoveAll => {
            Err(ControlError::Invalid("Unexpected wildcard response"))
        }
    }
}
