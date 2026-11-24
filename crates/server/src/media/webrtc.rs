use super::{
    AttachmentMode, AttachmentReceipt, Completion, DigitSupport, MediaHub, MediaPort, Port,
};
use crate::{webrtc::WebRtcSession, CallKey};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::MediaError;
use tokio::sync::oneshot;

pub(super) struct Attachment {
    key: CallKey,
    session: WebRtcSession,
    direction: PeerMediaDirection,
    mode: AttachmentMode,
    reply: Completion<()>,
}

impl MediaPort {
    pub async fn attach_webrtc(
        &self,
        key: CallKey,
        session: WebRtcSession,
        direction: PeerMediaDirection,
    ) -> Result<(), MediaError> {
        self.queue_webrtc_attachment(key, session, direction, AttachmentMode::Answered)?
            .wait()
            .await
    }

    pub(crate) fn queue_webrtc_attachment(
        &self,
        key: CallKey,
        session: WebRtcSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
    ) -> Result<AttachmentReceipt, MediaError> {
        let (reply, response) = oneshot::channel();
        self.webrtc
            .try_send(Attachment {
                key,
                session,
                direction,
                mode,
                reply: Completion::Reply(reply),
            })
            .map_err(|_| MediaError::Capacity)?;
        Ok(AttachmentReceipt { response })
    }

    // The media task reports the result of this attachment as a notice.
    pub(crate) fn start_webrtc_attachment(
        &self,
        key: CallKey,
        session: WebRtcSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
        serial: u64,
    ) -> Result<(), MediaError> {
        self.webrtc
            .try_send(Attachment {
                key,
                session,
                direction,
                mode,
                reply: Completion::Notice(serial),
            })
            .map_err(|_| MediaError::Capacity)
    }
}

impl MediaHub {
    fn attach_webrtc(
        &mut self,
        key: CallKey,
        session: WebRtcSession,
        direction: PeerMediaDirection,
        mode: AttachmentMode,
    ) -> Result<(), MediaError> {
        self.check_link(&key)?;
        let support = session
            .telephone()
            .map(DigitSupport::Rfc4733)
            .unwrap_or(DigitSupport::Unsupported);
        let (port, channels) = Port::new(direction, support);
        self.install_link(key.clone(), port, mode.gate(direction))?;
        self.workers
            .spawn(async move { (key, session.run(channels).await.map(|stats| stats.audio)) });
        Ok(())
    }

    pub(super) fn webrtc_attachment(&mut self, attachment: Attachment) -> Result<(), MediaError> {
        let Attachment {
            key,
            session,
            direction,
            mode,
            reply,
        } = attachment;
        if !reply.abandoned() {
            let result = self.attach_webrtc(key.clone(), session, direction, mode);
            self.complete(&key, result, reply)?;
        }
        Ok(())
    }
}
