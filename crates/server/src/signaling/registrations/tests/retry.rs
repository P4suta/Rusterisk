use super::{config, stream::reply};
use crate::{
    media::MediaHub,
    signaling::{cleanup, transport::Input, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::{HeadersExt, Request, SipMessage};
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;
mod stream;

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
    async fn new(count: usize) -> Result<Self, ControlError> {
        let registrar = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let remote = registrar.local_addr().map_err(failure)?.to_string();
        let targets = [remote.as_str(); 2];
        let mut config = config(targets.get(..count).ok_or(ControlError::Capacity)?, 0)?;
        for settings in &mut config.registrations {
            settings.retry_interval = 2;
            settings.max_retries = 1;
        }
        let uri = config
            .registrations
            .first()
            .ok_or(ControlError::Capacity)?
            .server_uri
            .as_str()
            .to_owned();
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

    async fn request(
        &mut self,
        now: u64,
        sequence: u32,
        account: usize,
    ) -> Result<Request, ControlError> {
        self.signaling.registration_tick(now).await?;
        let mut bytes = [0; 16_385];
        let (size, peer) = tokio::time::timeout(
            Duration::from_millis(250),
            self.registrar.recv_from(&mut bytes),
        )
        .await
        .map_err(failure)?
        .map_err(failure)?;
        let source = std::str::from_utf8(&bytes[..size]).map_err(failure)?;
        if peer != self.local || !source.starts_with(&format!("REGISTER {} SIP/2.0\r\n", self.uri))
        {
            return Err(ControlError::Invalid(
                "Registration retry changed its signaling route or URI",
            ));
        }
        let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected registration retry request"));
        };
        if request.cseq_header().map_err(failure)?.value() != format!("{sequence} REGISTER")
            || request.to_header().map_err(failure)?.value()
                != format!("<sip:account{account}@provider.example>")
        {
            return Err(ControlError::Invalid(
                "Registration retry reserved a sequence before dispatch",
            ));
        }
        Ok(request)
    }

    async fn response(
        &mut self,
        request: &Request,
        status: u16,
        headers: &str,
        now: u64,
    ) -> Result<(), ControlError> {
        let bytes = reply(request, status, headers)?;
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
                "Expected registration retry response",
            ));
        };
        if !self.signaling.registration_received(&packet, now) {
            return Err(ControlError::Invalid(
                "Registration retry response was not consumed",
            ));
        }
        Ok(())
    }

    async fn grant(
        &mut self,
        request: &Request,
        expiration: u32,
        now: u64,
    ) -> Result<(), ControlError> {
        let contact = request.contact_header().map_err(failure)?.value();
        self.response(
            request,
            200,
            &format!("Contact: {contact};expires={expiration}\r\n"),
            now,
        )
        .await
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
                "Registration retried before its deadline or after exhaustion",
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

#[tokio::test]
async fn polling_before_an_owned_deadline_never_creates_a_request() -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        if driver
            .signaling
            .registrations
            .poll(0, driver.local, 999)?
            .is_some()
        {
            return Err(ControlError::Invalid(
                "Polling created a request before the initial deadline",
            ));
        }
        let initial = driver.request(1000, 1, 0).await?;
        driver
            .response(&initial, 500, "Retry-After: 1\r\n", 1100)
            .await?;
        if driver
            .signaling
            .registrations
            .poll(0, driver.local, 2099)?
            .is_some()
        {
            return Err(ControlError::Invalid(
                "Polling created a request before the retry deadline",
            ));
        }
        let retried = driver.request(2100, 2, 0).await?;
        driver.grant(&retried, 11, 2200).await?;
        if driver
            .signaling
            .registrations
            .poll(0, driver.local, 3199)?
            .is_some()
        {
            return Err(ControlError::Invalid(
                "Polling created a request before the refresh deadline",
            ));
        }
        driver.request(3200, 3, 0).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}

#[tokio::test]
async fn positive_retry_after_does_not_consume_or_require_configured_retry_budget(
) -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        let initial = driver.request(1000, 1, 0).await?;
        driver
            .response(&initial, 500, "Retry-After: 1 (busy);duration=5\r\n", 1100)
            .await?;
        driver.silent(2099).await?;
        let hinted = driver.request(2100, 2, 0).await?;
        driver.response(&hinted, 500, "", 2200).await?;
        driver.silent(4199).await?;
        let configured = driver.request(4200, 3, 0).await?;
        driver
            .response(&configured, 500, "Retry-After: 1\r\n", 4300)
            .await?;
        let hinted = driver.request(5300, 4, 0).await?;
        driver.response(&hinted, 500, "", 5400).await?;
        driver.silent(7400).await?;
        driver.silent(37_400).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}

#[tokio::test]
async fn configured_failure_retries_stop_at_the_maximum() -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        let initial = driver.request(1000, 1, 0).await?;
        driver.response(&initial, 500, "", 1100).await?;
        driver.silent(3099).await?;
        let retry = driver.request(3100, 2, 0).await?;
        driver.response(&retry, 500, "", 3200).await?;
        driver.silent(5200).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}

#[tokio::test]
async fn successful_grants_reset_the_configured_retry_counter() -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        let initial = driver.request(1000, 1, 0).await?;
        driver.response(&initial, 500, "", 1100).await?;
        let retry = driver.request(3100, 2, 0).await?;
        driver.grant(&retry, 11, 3200).await?;
        let refresh = driver.request(4200, 3, 0).await?;
        driver.response(&refresh, 500, "", 4300).await?;
        let retry = driver.request(6300, 4, 0).await?;
        driver.response(&retry, 500, "", 6400).await?;
        driver.silent(8400).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}

#[tokio::test]
async fn a_backoff_allows_another_address_of_record_without_reserving_its_sequence(
) -> Result<(), ControlError> {
    let mut driver = Driver::new(2).await?;
    let primary = async {
        let first = driver.request(1000, 1, 0).await?;
        driver
            .response(&first, 500, "Retry-After: 5\r\n", 1100)
            .await?;
        let second = driver.request(1200, 2, 1).await?;
        driver.grant(&second, 3600, 1300).await?;
        driver.silent(6099).await?;
        let retried = driver.request(6100, 3, 0).await?;
        driver.grant(&retried, 3600, 6200).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}

#[tokio::test]
async fn cancellation_absorbs_received_retry_instructions() -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        let initial = driver.request(1000, 1, 0).await?;
        driver
            .response(&initial, 500, "Retry-After: 1\r\n", 1100)
            .await?;
        driver.signaling.registrations.cancel();
        driver.silent(2100).await?;
        driver.silent(u64::MAX).await?;
        if driver
            .signaling
            .registrations
            .poll(0, driver.local, u64::MAX)?
            .is_some()
        {
            return Err(ControlError::Invalid(
                "Cancelled registration created a retry request",
            ));
        }
        Ok(())
    }
    .await;
    driver.finish(primary).await
}

#[tokio::test]
async fn malformed_retry_instructions_never_fall_back_to_an_earlier_attempt(
) -> Result<(), ControlError> {
    for headers in [
        "Retry-After: 1 (unterminated\r\n",
        "Retry-After: 1;duration=\"2\"\r\n",
        "Retry-After: 1\r\nRetry-After: 2\r\n",
    ] {
        let mut driver = Driver::new(1).await?;
        let primary = async {
            let initial = driver.request(1000, 1, 0).await?;
            driver.response(&initial, 500, headers, 1100).await?;
            driver.silent(2100).await?;
            driver.silent(3100).await?;
            driver.silent(33_000).await?;
            Ok(())
        }
        .await;
        driver.finish(primary).await?;
    }
    Ok(())
}

#[tokio::test]
async fn timeouts_wait_for_the_configured_interval_and_stop_after_exhaustion(
) -> Result<(), ControlError> {
    let mut driver = Driver::new(1).await?;
    let primary = async {
        driver.request(1000, 1, 0).await?;
        driver.silent(33_000).await?;
        driver.silent(34_999).await?;
        driver.request(35_000, 2, 0).await?;
        driver.silent(67_000).await?;
        driver.silent(69_000).await?;
        Ok(())
    }
    .await;
    driver.finish(primary).await
}
