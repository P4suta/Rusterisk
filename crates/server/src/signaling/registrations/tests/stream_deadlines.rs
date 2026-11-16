use super::stream::{configuration, receive, reply};
use crate::{
    media::MediaHub,
    signaling::{cleanup, registrations::State, transport::Input, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::HeadersExt;
use rusterisk_sip::SipStreamDecoder;
use std::time::Duration;
use tokio::{io::AsyncWriteExt, net::TcpListener};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn initiated_registration_waits_for_a_response_beyond_the_accepted_first_message_deadline(
) -> Result<(), ControlError> {
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let uri = format!(
        "sip:{};transport=tcp",
        registrar.local_addr().map_err(failure)?
    );
    let config = configuration(&uri)?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller, effects, media, notices).await?;
    let primary: Result<(), ControlError> = async {
        signaling.registration_tick(1000).await?;
        let completed =
            tokio::time::timeout(Duration::from_secs(3), signaling.registrations.completed())
                .await
                .map_err(failure)??
                .ok_or(ControlError::Capacity)?;
        signaling.registration_completed(completed, 1000)?;
        let Input::Prepared(prepared) =
            tokio::time::timeout(Duration::from_secs(3), signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid("Expected registration preparation"));
        };
        let (mut peer, _address) =
            tokio::time::timeout(Duration::from_secs(3), registrar.accept())
                .await
                .map_err(failure)?
                .map_err(failure)?;
        signaling.registration_prepared(prepared, 1000)?;
        signaling.registration_tick(1000).await?;
        let mut decoder = SipStreamDecoder::new();
        let initial = receive(&mut peer, &mut decoder).await?;
        tokio::select! {
            event = signaling.network.next() => {
                return match event? {
                    Input::Closed { result: Err(_), .. } => Err(ControlError::Invalid("Initiated registration expired before its response deadline")),
                    Input::Closed { result: Ok(()), .. } | Input::Packet(_) | Input::Prepared(_) | Input::SetupFinished { .. } => Err(ControlError::Invalid("Unexpected event while awaiting registration response")),
                };
            }
            () = tokio::time::sleep(Duration::from_secs(6)) => {}
        }
        if !matches!(signaling.registrations.clients[0].state, State::Trying { .. }) {
            return Err(ControlError::Invalid("Registration left its transaction wait"));
        }
        let contact = initial.request.contact_header().map_err(failure)?.value();
        let grant = reply(
            &initial.request,
            200,
            &format!("Contact: {contact};expires=11\r\nExpires: 11\r\n"),
        )?;
        peer.write_all(grant.as_bytes()).await.map_err(failure)?;
        let Input::Packet(packet) =
            tokio::time::timeout(Duration::from_secs(3), signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid("Expected delayed registration grant"));
        };
        if !signaling.registration_received(&packet, 7001)
            || !matches!(signaling.registrations.clients[0].state, State::Registered { .. })
        {
            return Err(ControlError::Invalid("Delayed registration grant was discarded"));
        }
        signaling.registration_tick(8001).await?;
        let refresh = receive(&mut peer, &mut decoder).await?;
        if refresh.request.cseq_header().map_err(failure)?.value() != "2 REGISTER" {
            return Err(ControlError::Invalid("Delayed grant changed refresh sequencing"));
        }
        Ok(())
    }
    .await;
    let result = cleanup::combine(primary, signaling.finish_registrations().await);
    cleanup::combine(result, signaling.network.close().await)
}
