use super::{failure, Signaling};
use crate::signaling::calls;
use crate::{
    media::{AttachmentMode, AttachmentRelease, Order},
    rtp::{RtpSession, SocketPair},
    CallKey, ControlError,
};
use rusterisk_api::{AudioSocketTarget, ExternalRtpTarget, StaticRtpCodec};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::AudioFormat;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::{io::AsyncWriteExt, sync::watch};

pub(super) struct PendingStream {
    pub(super) cancel: watch::Sender<bool>,
    pub(super) received: bool,
}

pub(super) struct StreamCompletion {
    key: CallKey,
    result: Result<(), ControlError>,
}

impl Signaling {
    pub(super) fn start_audio_socket(
        &mut self,
        key: CallKey,
        target: AudioSocketTarget,
    ) -> Result<(), ControlError> {
        if self.records.iter(calls::ACTIVE).count()
            + self.records.iter(calls::EXTERNAL).count()
            + self.records.iter(calls::STREAM).count()
            + self.records.iter(calls::WEBSOCKET).count()
            >= 64
            || self.stream_jobs.len() >= 64
        {
            return Err(ControlError::Capacity);
        }
        let (cancel, mut stopped) = watch::channel(false);
        let media = self.media.clone();
        self.records.insert(
            calls::STREAM,
            key.clone(),
            PendingStream {
                cancel,
                received: false,
            },
        )?;
        self.stream_jobs.spawn(async move {
            let connection = async {
                let mut socket = tokio::net::TcpStream::connect(target.remote()).await.map_err(failure)?;
                socket.set_nodelay(true).map_err(failure)?;
                let identity = rusterisk_media::AudioSocket::Uuid(target.session()).encode().map_err(failure)?;
                socket.write_all(&identity).await.map_err(failure)?;
                media.attach_audiosocket(key.clone(), socket, 8000).await.map_err(failure)
            };
            let mut result = tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(4), connection) => result.map_err(failure).and_then(|result| result),
                _changed = stopped.changed() => Err(ControlError::Adapter("AudioSocket admission cancelled".into())),
            };
            if let Err(primary) = &result {
                if let Err(cleanup) = media.release(key.clone()) { result = Err(failure(format!("{primary}; stream admission cleanup failed: {cleanup}"))); }
            }
            StreamCompletion { key, result }
        });
        Ok(())
    }

    pub(super) async fn audio_socket_completed(
        &mut self,
        completed: StreamCompletion,
    ) -> Result<(), ControlError> {
        let Some(pending) = self.records.remove(calls::STREAM, &completed.key) else {
            self.order(Order::Release(completed.key))?;
            return Ok(());
        };
        let admitted = completed.result.is_ok();
        if admitted {
            self.records
                .insert(calls::EXTERNAL, completed.key.clone(), ())?;
        }
        self.notify(crate::Signal::AudioSocketAdmitted {
            key: completed.key.clone(),
            result: completed.result,
        })
        .await?;
        if admitted {
            self.notify(crate::Signal::DtmfReady {
                key: completed.key.clone(),
                events: u16::MAX,
            })
            .await?;
            self.notify(crate::Signal::Milestone {
                key: completed.key.clone(),
                milestone: crate::Milestone::Answered,
            })
            .await?;
            if pending.received {
                self.notify(crate::Signal::Milestone {
                    key: completed.key,
                    milestone: crate::Milestone::MediaConnected,
                })
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn finish_streams(&mut self) -> Result<(), ControlError> {
        for (_, pending) in self.records.drain(calls::STREAM) {
            pending.cancel.send_replace(true);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.stream_jobs.is_empty() {
            match tokio::time::timeout_at(deadline, self.stream_jobs.join_next()).await {
                Ok(Some(Ok(completed))) => {
                    if let Err(error) = completed.result {
                        tracing::debug!(channel = %completed.key.id, generation = completed.key.generation, %error, "stream admission stopped during shutdown");
                    }
                }
                Ok(Some(Err(error))) => return Err(failure(error)),
                Ok(None) => break,
                Err(error) => {
                    self.stream_jobs.abort_all();
                    while let Some(result) = self.stream_jobs.join_next().await {
                        match result {
                            Ok(_) => {}
                            Err(error) if error.is_cancelled() => {}
                            Err(error) => return Err(failure(error)),
                        }
                    }
                    return Err(failure(error));
                }
            }
        }
        Ok(())
    }

    // Queues the attachment, whose result the media task reports as a notice.
    pub(super) fn start_external_rtp(
        &mut self,
        key: CallKey,
        target: ExternalRtpTarget,
    ) -> Result<(SocketAddr, AttachmentRelease), ControlError> {
        if !self.owed.is_empty() {
            return Err(ControlError::Capacity);
        }
        if self.records.iter(calls::ACTIVE).count()
            + self.records.iter(calls::EXTERNAL).count()
            + self.external_waits()
            + self.records.iter(calls::STREAM).count()
            + self.records.iter(calls::WEBSOCKET).count()
            >= 64
        {
            return Err(ControlError::Capacity);
        }
        let ip = match target.remote().ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let route = std::net::UdpSocket::bind(SocketAddr::new(ip, 0)).map_err(failure)?;
        route.connect(target.remote()).map_err(failure)?;
        let ip = route.local_addr().map_err(failure)?.ip();
        drop(route);
        let sockets = SocketPair::bind_for_slot(
            ip,
            self.config.rtp.start,
            self.config.rtp.end,
            key.handle().slot(),
        )
        .map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let format = match target.codec() {
            StaticRtpCodec::Pcmu => AudioFormat::Pcmu,
            StaticRtpCodec::Pcma => AudioFormat::Pcma,
        };
        let serial = self.next_serial();
        let release = self
            .media
            .start_attachment(
                key,
                sockets,
                RtpSession {
                    remote: target.remote(),
                    format,
                    payload_type: target.codec().payload_type(),
                    telephone: None,
                },
                PeerMediaDirection::SendReceive,
                AttachmentMode::Answered,
                serial,
            )
            .map_err(ControlError::from)?;
        Ok((local, release))
    }
}

#[test]
fn a_closed_media_service_has_a_distinct_admission_error() {
    use rusterisk_media::MediaError;
    assert_eq!(
        ControlError::from(MediaError::ServiceClosed),
        ControlError::ServiceClosed
    );
    assert_eq!(
        ControlError::from(MediaError::Capacity),
        ControlError::Capacity
    );
}
