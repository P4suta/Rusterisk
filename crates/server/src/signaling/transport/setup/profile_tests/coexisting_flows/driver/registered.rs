use super::{failure, require, Fixture};
use crate::{Action, ControlError, Credentials};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    HeadersExt, Method, Request, Response, SipMessage, Uri,
};
use rusterisk_api::ChannelId;
use rusterisk_sip::{parse_registration_contacts, RegistrationContacts};
use std::{net::SocketAddr, time::Duration};

fn registration(
    uri: &Uri,
    peer: SocketAddr,
    contact: &str,
    name: &str,
    sequence: u32,
    authorization: &str,
) -> String {
    format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/TLS {peer};branch=z9hG4bK-{name}-{sequence};rport\r\nFrom: <sips:phone@localhost>;tag={name}\r\nTo: <sips:phone@localhost>\r\nCall-ID: {name}\r\nCSeq: {sequence} REGISTER\r\nContact: {contact}\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
}

fn response(
    bytes: &[u8],
    name: &str,
    sequence: u32,
    status: u16,
) -> Result<Response, ControlError> {
    let SipMessage::Response(response) = SipMessage::try_from(bytes).map_err(failure)? else {
        return Err(ControlError::Invalid(
            "Expected a response on the registered TLS flow",
        ));
    };
    require(
        response.status_code.code() == status
            && response.call_id_header().map_err(failure)?.value() == name
            && response.cseq_header().map_err(failure)?.value() == format!("{sequence} REGISTER"),
        "TLS registration response changed its status or transaction identity",
    )?;
    Ok(response)
}

async fn registered(
    fixture: &mut Fixture,
    index: usize,
    name: &str,
    preference: u16,
) -> Result<Uri, ControlError> {
    let peer = fixture.peer.local_addr().map_err(failure)?;
    let uri = Uri::try_from(format!("sips:{}", fixture.local)).map_err(failure)?;
    let contact = Uri::try_from(format!("sips:{name}@{peer};transport=TCP")).map_err(failure)?;
    let contact_header = format!("<{contact}>;q=0.{preference:03}");
    let initial = registration(&uri, peer, &contact_header, name, 1, "");
    let challenge = response(
        &fixture.exchange(index, initial.as_bytes()).await?,
        name,
        1,
        401,
    )?;
    let challenge = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(ControlError::Invalid(
                "TLS registration omitted its authentication challenge",
            ))?
            .value(),
    )
    .map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "registered-flow-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "signaling-secret",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"registered-flow-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    let authenticated = registration(&uri, peer, &contact_header, name, 2, &authorization);
    let accepted = response(
        &fixture.exchange(index, authenticated.as_bytes()).await?,
        name,
        2,
        200,
    )?;
    let RegistrationContacts::Bindings(contacts) = parse_registration_contacts(
        accepted
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
            .map(|header| header.value()),
        None,
    )
    .map_err(failure)?
    else {
        return Err(ControlError::Invalid(
            "TLS registration did not return its accepted bindings",
        ));
    };
    require(
        contacts.as_slice().len() == index + 1
            && contacts
                .as_slice()
                .iter()
                .any(|binding| binding.uri() == &contact && binding.preference() == preference),
        "TLS registration lost a contact URI, preference, or coexisting binding",
    )?;
    Ok(contact)
}

fn request(bytes: &[u8]) -> Result<Request, ControlError> {
    match SipMessage::try_from(bytes).map_err(failure)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ControlError::Invalid(
            "Expected a request on the selected registered TLS flow",
        )),
    }
}

async fn selected_invitation(fixture: &mut Fixture) -> Result<Request, ControlError> {
    let [old, new] = &mut fixture.flows;
    let old = old.as_mut().ok_or(ControlError::Capacity)?;
    let new = new.as_mut().ok_or(ControlError::Capacity)?;
    let (index, bytes) = tokio::select! {
        bytes = old.message() => (0, bytes?),
        bytes = new.message() => (1, bytes?),
    };
    require(
        index == 0,
        "Registered origination reached the other same-peer TLS flow",
    )?;
    let invite = request(&bytes)?;
    require(
        invite.method == Method::Invite,
        "Selected registered TLS flow did not receive its INVITE",
    )?;
    Ok(invite)
}

async fn refuse_invitation(fixture: &mut Fixture, invite: &Request) -> Result<(), ControlError> {
    let wire = format!("SIP/2.0 486 Busy Here\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=registered-peer\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", invite.top_via_header().map_err(failure)?.value(), invite.from_header().map_err(failure)?.value(), invite.to_header().map_err(failure)?.value(), invite.call_id_header().map_err(failure)?.value(), invite.cseq_header().map_err(failure)?.value());
    let ack = request(&fixture.exchange(0, wire.as_bytes()).await?)?;
    let sequence = invite
        .cseq_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    require(
        ack.method == Method::Ack
            && ack.uri == invite.uri
            && ack.call_id_header().map_err(failure)?.value()
                == invite.call_id_header().map_err(failure)?.value()
            && ack.cseq_header().map_err(failure)?.value() == format!("{} ACK", sequence.seq),
        "Registered origination cleanup changed its captured flow or transaction",
    )
}

async fn probe(fixture: &mut Fixture, index: usize, name: &str) -> Result<(), ControlError> {
    let peer = fixture.peer.local_addr().map_err(failure)?;
    let wire = format!("OPTIONS sips:server@localhost SIP/2.0\r\nVia: SIP/2.0/TLS {peer};branch=z9hG4bK-{name}\r\nFrom: <sips:probe@localhost>;tag={name}\r\nTo: <sips:server@localhost>\r\nCall-ID: {name}\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
    let bytes = fixture.exchange(index, wire.as_bytes()).await?;
    let SipMessage::Response(response) = SipMessage::try_from(bytes.as_slice()).map_err(failure)?
    else {
        return Err(ControlError::Invalid(
            "A registered INVITE was duplicated before the flow probe response",
        ));
    };
    require(
        response.status_code.code() == 200
            && response.call_id_header().map_err(failure)?.value() == name
            && response.cseq_header().map_err(failure)?.value() == "1 OPTIONS",
        "TLS flow probe received another flow's response or queued duplicate",
    )
}

#[tokio::test]
async fn actual_registered_origination_keeps_the_selected_contact_and_tls_flow(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary = async {
        fixture.prepare_flows().await?;
        fixture.start()?;
        let selected = registered(&mut fixture, 0, "registered-old", 900).await?;
        let other = registered(&mut fixture, 1, "registered-new", 100).await?;
        require(
            selected != other,
            "TLS fixture registered identical contact URIs",
        )?;
        tokio::time::timeout(
            Duration::from_secs(2),
            fixture.controller.execute(
                Credentials::new("app".into(), "test".into())?,
                None,
                Action::Originate {
                    id: ChannelId::try_from("registered-flow-origination".to_owned())?,
                    endpoint: "PJSIP/phone".into(),
                    app: "app".into(),
                    args: Vec::new(),
                    caller: "displayed-caller".into(),
                    timeout: 30,
                },
            ),
        )
        .await
        .map_err(failure)??;
        let invite = selected_invitation(&mut fixture).await?;
        require(
            invite.uri == selected,
            "Registered origination did not preserve its selected Contact URI",
        )?;
        refuse_invitation(&mut fixture, &invite).await?;
        probe(&mut fixture, 1, "registered-new-probe").await?;
        probe(&mut fixture, 0, "registered-old-probe").await?;
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}
