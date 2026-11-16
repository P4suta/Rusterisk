use super::{DigitSupport, MediaHub, MediaPort, Port};
use crate::{webrtc::WebRtcSession, CallKey};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::MediaError;
use std::time::Duration;
use tokio::sync::oneshot;

pub(super) struct Attachment {
    key: CallKey,
    session: WebRtcSession,
    direction: PeerMediaDirection,
    reply: oneshot::Sender<Result<(), MediaError>>,
}

impl MediaPort {
    pub async fn attach_webrtc(
        &self,
        key: CallKey,
        session: WebRtcSession,
        direction: PeerMediaDirection,
    ) -> Result<(), MediaError> {
        let (reply, response) = oneshot::channel();
        self.webrtc
            .try_send(Attachment {
                key,
                session,
                direction,
                reply,
            })
            .map_err(|_| MediaError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|error| MediaError::Transport(error.to_string()))?
            .map_err(|error| MediaError::Transport(error.to_string()))?
    }
}

impl MediaHub {
    fn attach_webrtc(
        &mut self,
        key: CallKey,
        session: WebRtcSession,
        direction: PeerMediaDirection,
    ) -> Result<(), MediaError> {
        self.check_link(&key)?;
        let support = session
            .telephone()
            .map(DigitSupport::Rfc4733)
            .unwrap_or(DigitSupport::Unsupported);
        let (port, channels) = Port::new(direction, support);
        self.install_link(key.clone(), port, direction)?;
        self.workers
            .spawn(async move { (key, session.run(channels).await.map(|stats| stats.audio)) });
        Ok(())
    }

    pub(super) fn webrtc_attachment(&mut self, attachment: Attachment) -> Result<(), MediaError> {
        let Attachment {
            key,
            session,
            direction,
            reply,
        } = attachment;
        if !reply.is_closed() {
            let result = self.attach_webrtc(key.clone(), session, direction);
            let admitted = result.is_ok();
            if reply.send(result).is_err() && admitted {
                self.remove(&key)?;
            }
        }
        Ok(())
    }
}
