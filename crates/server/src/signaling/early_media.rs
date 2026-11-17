use super::{cleanup, failure, media, MediaAdmission, PendingWebRtc, PreparedAnswer, Signaling};
use crate::{
    media::{AttachmentMode, AttachmentReceipt},
    rtp::SocketPair,
    CallKey, ControlError, Signal,
};
use rusterisk_sip::{Session, SessionSecurity, WebRtcDescription};
use std::{net::SocketAddr, time::Instant};

impl Signaling {
    pub(super) fn incoming_answer(
        &mut self,
        key: &CallKey,
        now: u64,
    ) -> Result<Option<(usize, rusterisk_sip::Output)>, ControlError> {
        let call = self
            .calls
            .get(key)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let listener = call.listener;
        if self.listeners[listener]
            .engine
            .active_session(key.id.as_str(), key.generation)
            .is_some()
        {
            return Ok(None);
        }
        if let MediaAdmission::Early(answer) = &call.media {
            let output = answer
                .answer(&mut self.listeners[listener].engine, key, now)
                .map_err(failure)?;
            return Ok(Some((listener, output)));
        }
        let (pending, answer, _) = self.incoming_media(key)?;
        let output = answer
            .answer(&mut self.listeners[listener].engine, key, now)
            .map_err(failure)?;
        self.calls
            .get_mut(key)
            .ok_or(ControlError::NotFound("Channel not found"))?
            .media = pending;
        Ok(Some((listener, output)))
    }

    fn queue_early_attachment(
        &self,
        key: &CallKey,
    ) -> Result<(AttachmentReceipt, PreparedAnswer), ControlError> {
        let (admission, answer, offered) = self.incoming_media(key)?;
        let receipt = admission
            .attach(&self.media, key.clone(), &offered, AttachmentMode::Early)
            .map_err(failure)?;
        Ok((receipt, answer))
    }

    pub(super) fn incoming_media(
        &self,
        key: &CallKey,
    ) -> Result<(MediaAdmission, PreparedAnswer, Session), ControlError> {
        let listener = self
            .calls
            .get(key)
            .ok_or(ControlError::NotFound("Channel not found"))?
            .listener;
        let transport = self
            .config
            .transports
            .iter()
            .find(|transport| transport.name == self.listeners[listener].name)
            .ok_or(ControlError::NotFound("Transport not found"))?;
        let incoming = self.listeners[listener]
            .engine
            .pending(key.id.as_str())
            .ok_or(ControlError::NotFound("Incoming offer not found"))?;
        let offered = incoming.session.clone();
        let bind = if transport.bind.ip().is_unspecified() {
            media::local_ip(incoming.peer)?
        } else {
            transport.bind.ip()
        };
        let sockets =
            SocketPair::bind(bind, self.config.rtp.start, self.config.rtp.end).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let advertised = SocketAddr::new(
            transport.external_media_address.unwrap_or(local.ip()),
            local.port(),
        );
        let (admission, answer) = match &offered.security {
            SessionSecurity::Rtp => (
                MediaAdmission::Rtp(sockets),
                PreparedAnswer::Rtp(advertised),
            ),
            SessionSecurity::WebRtc(description) => {
                let (session, answer) = crate::webrtc::WebRtcSession::answer(
                    sockets,
                    advertised,
                    description.as_str(),
                    &[offered.codec],
                    Instant::now(),
                )
                .map_err(failure)?;
                let description = WebRtcDescription::parse(&answer).map_err(failure)?;
                (
                    MediaAdmission::WebRtc(PendingWebRtc::Session(session)),
                    PreparedAnswer::WebRtc {
                        local: advertised,
                        description,
                    },
                )
            }
        };
        Ok((admission, answer, offered))
    }

    pub(super) async fn prepare_early_media(
        &mut self,
        key: &CallKey,
        now: u64,
    ) -> Result<(), ControlError> {
        let call = self
            .calls
            .get(key)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let listener = call.listener;
        match &call.media {
            MediaAdmission::Early(_) | MediaAdmission::Attached => return Ok(()),
            MediaAdmission::Rtp(_) | MediaAdmission::WebRtc(_) => {
                return Err(ControlError::Unsupported)
            }
            MediaAdmission::Incoming => {}
        }
        let (receipt, answer) = self.queue_early_attachment(key)?;
        receipt.wait().await.map_err(failure)?;
        let progress = match answer.progress(&mut self.listeners[listener].engine, key, now) {
            Ok(output) => output,
            Err(error) => {
                return cleanup::combine(
                    Err(failure(error)),
                    self.media.release(key.clone()).map_err(failure),
                )
            }
        };
        let Some(call) = self.calls.get_mut(key) else {
            return cleanup::combine(
                Err(ControlError::NotFound(
                    "Channel generation no longer active",
                )),
                self.media.release(key.clone()).map_err(failure),
            );
        };
        call.media = MediaAdmission::Early(answer);
        if let Err(error) = self.transmit(listener, progress.datagrams).await {
            let result =
                cleanup::combine(Err(error), self.media.release(key.clone()).map_err(failure));
            return cleanup::combine(
                result,
                self.notify(Signal::Terminated {
                    key: key.clone(),
                    cause: 41,
                })
                .await,
            );
        }
        Ok(())
    }
}
