use super::{config, stream::reply};
use crate::{
    media::MediaHub,
    signaling::{cleanup, transport::Input, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::{HeadersExt, Request, SipMessage};
use rusterisk_config::{Protocol, RegistrarUri};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct Driver {
    signaling: Signaling,
    registrar: UdpSocket,
    local: SocketAddr,
    uri: String,
}

impl Driver {
    async fn new(expiration: u32) -> Result<Self, ControlError> {
        let registrar = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let remote = registrar.local_addr().map_err(failure)?;
        let mut config = config(&[&remote.to_string()], 0)?;
        let uri = format!(
            "SIP:127.0.0.1:0{};transport=UDP;extension=%61b",
            remote.port()
        );
        let settings = config
            .registrations
            .first_mut()
            .ok_or(ControlError::Capacity)?;
        settings.expiration = expiration;
        settings.server_uri = RegistrarUri::parse(Protocol::Udp, &uri).map_err(failure)?;
        let (controller, _runner, effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        let signaling = Signaling::bind(config, controller, effects, media, notices).await?;
        let local = signaling
            .listeners()?
            .first()
            .ok_or(ControlError::Capacity)?
            .1;
        Ok(Self {
            signaling,
            registrar,
            local,
            uri,
        })
    }

    async fn request(&mut self, now: u64) -> Result<Request, ControlError> {
        self.signaling.registration_tick(now).await?;
        let mut bytes = [0; 16_385];
        let (size, peer) = tokio::time::timeout(
            Duration::from_millis(250),
            self.registrar.recv_from(&mut bytes),
        )
        .await
        .map_err(failure)?
        .map_err(failure)?;
        let wire = std::str::from_utf8(&bytes[..size]).map_err(failure)?;
        if peer != self.local || !wire.starts_with(&format!("REGISTER {} SIP/2.0\r\n", self.uri)) {
            return Err(ControlError::Invalid(
                "Registration correction changed its route or wire URI",
            ));
        }
        let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected REGISTER request"));
        };
        Ok(request)
    }

    async fn response(
        &mut self,
        request: &Request,
        status: u16,
        extra: &str,
        now: u64,
    ) -> Result<(), ControlError> {
        let bytes = reply(request, status, extra)?;
        let size = self
            .registrar
            .send_to(bytes.as_bytes(), self.local)
            .await
            .map_err(failure)?;
        if size != bytes.len() {
            return Err(ControlError::Invalid("Registrar response was truncated"));
        }
        let Input::Packet(packet) =
            tokio::time::timeout(Duration::from_millis(250), self.signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid(
                "Expected registration response packet",
            ));
        };
        if !self.signaling.registration_received(&packet, now) {
            return Err(ControlError::Invalid(
                "Registration response was not consumed",
            ));
        }
        Ok(())
    }

    async fn silent(&mut self, now: u64) -> Result<(), ControlError> {
        self.signaling.registration_tick(now).await?;
        match tokio::time::timeout(
            Duration::from_millis(30),
            self.registrar.recv_from(&mut [0; 16_385]),
        )
        .await
        {
            Err(_) => Ok(()),
            Ok(Err(error)) => Err(failure(error)),
            Ok(Ok(_)) => Err(ControlError::Invalid(
                "Registration sent an unexpected correction",
            )),
        }
    }

    async fn finish(mut self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let mut result = cleanup::combine(primary, self.signaling.finish_registrations().await);
        result = cleanup::combine(result, self.signaling.finish_native().await);
        result = cleanup::combine(result, self.signaling.network.close().await);
        drop(self.signaling);
        let rebound = UdpSocket::bind(self.local)
            .await
            .map(|_socket| ())
            .map_err(failure);
        cleanup::combine(result, rebound)
    }
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
            "Missing registration Digest attribute",
        ))
}

fn check(
    request: &Request,
    initial: &Request,
    uri: &str,
    expiration: u32,
    sequence: u32,
    count: u32,
) -> Result<(), ControlError> {
    for name in ["Contact", "Call-ID", "From", "To"] {
        let value = |request: &Request| {
            request
                .headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case(name))
                .map(|header| header.value().to_owned())
        };
        if value(request) != value(initial) {
            return Err(ControlError::Invalid(
                "Registration correction changed its identity or Contact",
            ));
        }
    }
    let expiration = expiration.to_string();
    if !request
        .headers
        .iter()
        .any(|header| header.name() == "Expires" && header.value() == expiration)
        || request.cseq_header().map_err(failure)?.value() != format!("{sequence} REGISTER")
    {
        return Err(ControlError::Invalid(
            "Registration correction changed its sequence or lifetime",
        ));
    }
    let authorization = request
        .authorization_header()
        .ok_or(ControlError::Invalid("Missing correction authorization"))?
        .value();
    let cnonce = quoted(authorization, "cnonce")?;
    let identity = hash("account:rusterisk:sample");
    let entity = hash("");
    let target = hash(&format!("REGISTER:{uri}:{entity}"));
    let expected = hash(&format!(
        "{identity}:minimum-nonce:{count:08x}:{cnonce}:auth-int:{target}"
    ));
    if quoted(authorization, "uri")? != uri
        || quoted(authorization, "response")? != expected
        || !authorization.contains(&format!("nc={count:08x}"))
    {
        return Err(ControlError::Invalid(
            "Registration correction changed its authenticated wire octets",
        ));
    }
    Ok(())
}

#[tokio::test]
async fn udp_corrections_raise_expiration_and_keep_the_learned_value_for_refreshes(
) -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        let initial = driver.request(1000).await?;
        driver.response(&initial, 401, "WWW-Authenticate: Digest realm=\"rusterisk\", nonce=\"minimum-nonce\", algorithm=SHA-256, qop=\"auth-int\"\r\n", 1100).await?;
        let authenticated = driver.request(1200).await?;
        check(&authenticated, &initial, &driver.uri, 1, 2, 1)?;
        driver.response(&authenticated, 423, "Min-Expires: 11\r\n", 1300).await?;
        let corrected = driver.request(1400).await?;
        check(&corrected, &initial, &driver.uri, 11, 3, 2)?;
        driver.response(&corrected, 423, "Min-Expires: 12\r\n", 1500).await?;
        let corrected = driver.request(1600).await?;
        check(&corrected, &initial, &driver.uri, 12, 4, 3)?;
        let contact = corrected.contact_header().map_err(failure)?.value();
        driver.response(&corrected, 200, &format!("Contact: {contact};expires=12\r\n"), 1700).await?;
        driver.silent(3699).await?;
        let refreshed = driver.request(3700).await?;
        check(&refreshed, &initial, &driver.uri, 12, 5, 4)?;
        if driver.signaling.registrations.clients[0].settings.expiration != 1 {
            return Err(ControlError::Invalid("Learned expiration changed the starting settings"));
        }
        Ok(())
    }.await;
    driver.finish(primary).await
}

#[tokio::test]
async fn invalid_minimum_values_and_removals_send_no_correction() -> Result<(), ControlError> {
    for (expiration, headers) in [
        (1, ""),
        (1, "Min-Expires: 0\r\n"),
        (1, "Min-Expires: 1\r\n"),
        (1, "Min-Expires: 11\r\nMin-Expires: 11\r\n"),
        (1, "Min-Expires: 4294967296\r\n"),
        (0, "Min-Expires: 11\r\n"),
    ] {
        let mut driver = Driver::new(expiration).await?;
        let primary = async {
            let initial = driver.request(1000).await?;
            driver.response(&initial, 423, headers, 1100).await?;
            driver.silent(1200).await?;
            Ok(())
        }
        .await;
        driver.finish(primary).await?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_before_the_corrected_request_is_polled_sends_no_bytes(
) -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        let initial = driver.request(1000).await?;
        driver
            .response(&initial, 423, "Min-Expires: 11\r\n", 1100)
            .await?;
        driver.signaling.registrations.cancel();
        driver.silent(1200).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}
