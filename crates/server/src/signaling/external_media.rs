use super::{failure, Signaling};
use crate::{
    rtp::{RtpSession, SocketPair},
    CallKey, ControlError,
};
use rusterisk_api::{AudioSocketTarget, ExternalRtpTarget, StaticRtpCodec};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{AudioFormat, MediaError};
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
        if self.calls.len()
            + self.external_media.len()
            + self.stream_admissions.len()
            + self.websocket_admissions.len()
            >= 64
            || self.stream_jobs.len() >= 64
        {
            return Err(ControlError::Capacity);
        }
        let (cancel, mut stopped) = watch::channel(false);
        let media = self.media.clone();
        self.stream_admissions.insert(
            key.clone(),
            PendingStream {
                cancel,
                received: false,
            },
        );
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
        let Some(pending) = self.stream_admissions.remove(&completed.key) else {
            self.media.release(completed.key).map_err(failure)?;
            return Ok(());
        };
        let admitted = completed.result.is_ok();
        if admitted {
            self.external_media.insert(completed.key.clone());
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
        for (_, pending) in std::mem::take(&mut self.stream_admissions) {
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

    pub(super) async fn connect_external_rtp(
        &mut self,
        key: CallKey,
        target: ExternalRtpTarget,
    ) -> Result<SocketAddr, ControlError> {
        if self.calls.len()
            + self.external_media.len()
            + self.stream_admissions.len()
            + self.websocket_admissions.len()
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
        let sockets =
            SocketPair::bind(ip, self.config.rtp.start, self.config.rtp.end).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let format = match target.codec() {
            StaticRtpCodec::Pcmu => AudioFormat::Pcmu,
            StaticRtpCodec::Pcma => AudioFormat::Pcma,
        };
        self.media
            .attach(
                key.clone(),
                sockets,
                RtpSession {
                    remote: target.remote(),
                    format,
                    payload_type: target.codec().payload_type(),
                    telephone: None,
                },
                PeerMediaDirection::SendReceive,
            )
            .await
            .map_err(|error| match error {
                MediaError::Capacity => ControlError::Capacity,
                error @ (MediaError::InvalidFrame
                | MediaError::InvalidPacket
                | MediaError::InvalidRoute
                | MediaError::UnknownStream
                | MediaError::Unsupported
                | MediaError::Codec(_)
                | MediaError::Transport(_)
                | MediaError::Control(_)) => failure(error),
            })?;
        self.external_media.insert(key);
        Ok(local)
    }
}
