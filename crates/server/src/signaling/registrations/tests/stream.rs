use crate::{
    media::MediaHub,
    signaling::{cleanup, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::{HeadersExt, Method, Request, SipMessage};
use rusterisk_config::Config;
use rusterisk_sip::{SipStreamDecoder, StreamItem};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn hash(source: &str) -> String {
    Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn quoted<'a>(source: &'a str, name: &str) -> Result<&'a str, ControlError> {
    source
        .split_once(&format!("{name}=\""))
        .and_then(|(_, value)| value.split_once('"'))
        .map(|(value, _)| value)
        .ok_or(ControlError::Invalid(
            "Missing registration authorization attribute",
        ))
}

pub(super) fn configuration(uri: &str) -> Result<Config, ControlError> {
    let uri = uri.replace(';', "\\;");
    Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", &format!("[native]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[account]\ntype=auth\nusername=account\npassword=sample\nrealm=rusterisk\n[provider]\ntype=registration\ntransport=native\noutbound_auth=account\nserver_uri={uri}\nclient_uri=sip:account@provider.test\ncontact_user=line\nmax_random_initial_delay=0\n"), "", "").map_err(failure)
}

async fn finish(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(timeout) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(timeout)),
                Err(error) => Err(failure(error)),
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(failure(timeout)),
            }
        }
    }
}

pub(super) struct Packet {
    pub(super) request: Request,
    pub(super) uri: String,
}

pub(super) async fn receive<S: AsyncRead + Unpin>(
    peer: &mut S,
    decoder: &mut SipStreamDecoder,
) -> Result<Packet, ControlError> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut byte = [0; 1];
        loop {
            if peer.read(&mut byte).await.map_err(failure)? == 0 {
                return Err(ControlError::Invalid("Registrar signaling flow closed"));
            }
            match decoder.push(&byte).map_err(failure)?.1 {
                Some(StreamItem::Message(bytes)) => {
                    let source = std::str::from_utf8(&bytes).map_err(failure)?;
                    let uri = source
                        .split_once("\r\n")
                        .and_then(|(line, _)| line.strip_prefix("REGISTER "))
                        .and_then(|line| line.strip_suffix(" SIP/2.0"))
                        .ok_or(ControlError::Invalid("Expected REGISTER request line"))?
                        .to_owned();
                    let SipMessage::Request(request) =
                        SipMessage::try_from(bytes.as_slice()).map_err(failure)?
                    else {
                        return Err(ControlError::Invalid("Expected REGISTER request"));
                    };
                    return Ok(Packet { request, uri });
                }
                Some(StreamItem::Crlf) | None => {}
            }
        }
    })
    .await
    .map_err(failure)?
}

pub(super) fn reply(request: &Request, status: u16, extra: &str) -> Result<String, ControlError> {
    Ok(format!("SIP/2.0 {status} Response\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=registrar\r\nCall-ID: {}\r\nCSeq: {}\r\n{extra}Content-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value()))
}

fn authorized(packet: &Packet, count: u32) -> Result<String, ControlError> {
    let value = packet
        .request
        .authorization_header()
        .ok_or(ControlError::Invalid("Missing registration authorization"))?
        .value();
    let nonce = quoted(value, "cnonce")?;
    let expected = hash(&format!(
        "{}:tcp-nonce:{count:08x}:{nonce}:auth-int:{}",
        hash("account:rusterisk:sample"),
        hash(&format!("REGISTER:{}:{}", packet.uri, hash("")))
    ));
    if !value.contains("algorithm=SHA-256")
        || !value.contains("qop=auth-int")
        || !value.contains(&format!("nc={count:08x}"))
        || quoted(value, "uri")? != packet.uri
        || quoted(value, "opaque")? != "tcp-opaque"
        || quoted(value, "response")? != expected
    {
        return Err(ControlError::Invalid(
            "Stream registration authorization changed wire octets",
        ));
    }
    Ok(nonce.into())
}

#[tokio::test]
async fn tcp_registration_authenticates_and_refreshes_on_one_owned_flow() -> Result<(), ControlError>
{
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let remote = registrar.local_addr().map_err(failure)?;
    let uri = format!(
        "SIP:127.0.0.1:0{};transport=TCP;extension=%61b",
        remote.port()
    );
    let config = configuration(&uri)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let stopped = controller.shutdown_signal();
    let task = tokio::spawn(signaling.run(stopped));
    let primary: Result<(), ControlError> = async {
        let (mut peer, _address) = tokio::time::timeout(Duration::from_secs(3), registrar.accept()).await.map_err(failure)?.map_err(failure)?;
        let mut decoder = SipStreamDecoder::new();
        let initial = receive(&mut peer, &mut decoder).await?;
        let contact = format!("<sip:line@{local};transport=tcp>");
        if initial.uri != uri || initial.request.method != Method::Register
            || initial.request.cseq_header().map_err(failure)?.value() != "1 REGISTER"
            || initial.request.contact_header().map_err(failure)?.value() != contact
            || !initial.request.top_via_header().map_err(failure)?.value().starts_with("SIP/2.0/TCP ")
        {
            return Err(ControlError::Invalid("Initial TCP registration changed its transport or URI"));
        }
        let challenge = reply(&initial.request, 401, "WWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"tcp-nonce\", algorithm=SHA-256, qop=\"auth-int\", opaque=\"tcp-opaque\"\r\n")?;
        peer.write_all(&challenge.as_bytes()[..17]).await.map_err(failure)?;
        if tokio::time::timeout(Duration::from_millis(50), peer.read(&mut [0; 1])).await.is_ok() {
            return Err(ControlError::Invalid("Registration processed an incomplete challenge"));
        }
        peer.write_all(&challenge.as_bytes()[17..]).await.map_err(failure)?;
        let retry = receive(&mut peer, &mut decoder).await?;
        let cnonce = authorized(&retry, 1)?;
        if retry.uri != uri
            || retry.request.cseq_header().map_err(failure)?.value() != "2 REGISTER"
            || retry.request.call_id_header().map_err(failure)?.value() != initial.request.call_id_header().map_err(failure)?.value()
        {
            return Err(ControlError::Invalid("TCP authentication changed the registration identity"));
        }
        let provisional = reply(&retry.request, 100, "")?;
        let grant = reply(&retry.request, 200, &format!("Contact: {contact};expires=11\r\nExpires: 11\r\n"))?;
        peer.write_all(format!("{provisional}{grant}").as_bytes()).await.map_err(failure)?;
        let refresh = receive(&mut peer, &mut decoder).await?;
        if refresh.uri != uri
            || refresh.request.cseq_header().map_err(failure)?.value() != "3 REGISTER"
            || refresh.request.call_id_header().map_err(failure)?.value() != initial.request.call_id_header().map_err(failure)?.value()
            || refresh.request.contact_header().map_err(failure)?.value() != contact
            || authorized(&refresh, 2)? != cnonce
        {
            return Err(ControlError::Invalid("TCP refresh changed its authenticated registration"));
        }
        if tokio::time::timeout(Duration::from_millis(100), registrar.accept()).await.is_ok() {
            return Err(ControlError::Invalid("Registration opened a second signaling flow"));
        }
        Ok(())
    }.await;
    controller.shutdown();
    let cleanup = finish(task).await;
    cleanup::combine(primary, cleanup)?;
    let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    Ok(())
}
