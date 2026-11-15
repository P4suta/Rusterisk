use crate::encryption::TlsContext;
use crate::{http, ControlError, Controller};
use axum::http::StatusCode;
use axum::Router;
use hyper::service::{service_fn, Service};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rusterisk_config::Http;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tower_http::timeout::TimeoutLayer;

pub struct HttpServer {
    listener: TcpListener,
    application: Router,
    encryption: Option<TlsContext>,
    controller: Controller,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FirstRequest {
    Waiting,
    Observed,
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn connection<I: AsyncRead + AsyncWrite + Send + Unpin + 'static>(
    io: I,
    application: Router,
    mut stop: watch::Receiver<bool>,
) -> Result<(), ControlError> {
    if *stop.borrow() {
        return Ok(());
    }
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(5))
        .max_headers(64)
        .max_buf_size(16384);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(16)
        .max_header_list_size(16384)
        .max_send_buf_size(65536);
    let (headers, mut header_receiver) = mpsc::channel(1);
    let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
        let application = application.clone();
        let headers = headers.clone();
        async move {
            match headers.try_send(()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(())) => {
                    tracing::trace!("Initial request marker already queued")
                }
                Err(mpsc::error::TrySendError::Closed(())) => {
                    tracing::trace!("Initial request marker already consumed")
                }
            }
            TowerToHyperService::new(application).call(request).await
        }
    });
    let connection = builder.serve_connection_with_upgrades(TokioIo::new(io), service);
    tokio::pin!(connection);
    let header_timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(header_timeout);
    let mut first = FirstRequest::Waiting;
    loop {
        tokio::select! {
            () = &mut header_timeout, if first == FirstRequest::Waiting => return Err(ControlError::Invalid("Initial request timed out")),
            marker = header_receiver.recv(), if first == FirstRequest::Waiting => {
                match marker { Some(()) => { first = FirstRequest::Observed; header_receiver.close(); }, None => return Err(ControlError::Invalid("Connection closed before request headers")) }
            }
            result = &mut connection => return result.map_err(failure),
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    connection.as_mut().graceful_shutdown();
                    return tokio::time::timeout(Duration::from_secs(5), &mut connection).await.map_err(failure)?.map_err(failure);
                }
            }
        }
    }
}

impl HttpServer {
    pub async fn bind(config: &Http, controller: Controller) -> Result<Self, ControlError> {
        if !config.bind.ip().is_loopback() && config.tls.is_none() {
            return Err(ControlError::Forbidden);
        }
        let encryption = match &config.tls {
            Some(files) => Some(TlsContext::load(files).await?),
            None => None,
        };
        let listener = TcpListener::bind(config.bind).await.map_err(failure)?;
        let scheme = match &encryption {
            Some(_) => http::HttpScheme::Https,
            None => http::HttpScheme::Http,
        };
        let application = http::router_for_scheme(controller.clone(), scheme).layer(
            TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(10)),
        );
        Ok(Self {
            listener,
            application,
            encryption,
            controller,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, ControlError> {
        self.listener.local_addr().map_err(failure)
    }

    pub fn with_media(mut self, media: crate::media::MediaPort) -> Self {
        let scheme = match &self.encryption {
            Some(_) => http::HttpScheme::Https,
            None => http::HttpScheme::Http,
        };
        self.application =
            http::router_with_media_for_scheme(self.controller.clone(), media, scheme).layer(
                TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    Duration::from_secs(10),
                ),
            );
        self
    }

    pub async fn run(self) -> Result<(), ControlError> {
        let mut stopped = self.controller.shutdown_signal();
        let mut connections = JoinSet::new();
        let result = loop {
            if *stopped.borrow() {
                break Ok(());
            }
            tokio::select! {
                changed = stopped.changed() => if changed.is_err() || *stopped.borrow() { break Ok(()); },
                accepted = self.listener.accept(), if connections.len() < 64 => {
                    let (stream, peer) = match accepted { Ok(accepted) => accepted, Err(error) => break Err(failure(error)) };
                    let encryption = self.encryption.clone();
                    let application = self.application.clone();
                    let stop = self.controller.shutdown_signal();
                    connections.spawn(async move {
                        let result = match encryption {
                            None => connection(stream, application, stop).await,
                            Some(encryption) => match encryption.accept(stream).await {
                                Ok(stream) => connection(stream, application, stop).await,
                                Err(error) => Err(error),
                            },
                        };
                        if let Err(error) = result { tracing::debug!(%peer, %error, "HTTP connection closed"); }
                    });
                }
                joined = connections.join_next(), if !connections.is_empty() => {
                    match joined {
                        Some(Ok(())) => {}
                        Some(Err(error)) => break Err(failure(error)),
                        None => break Err(ControlError::Adapter("Connection task missing".into())),
                    }
                }
            }
        };
        self.controller.shutdown();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut cleanup = Ok(());
        while !connections.is_empty() {
            match tokio::time::timeout_at(deadline, connections.join_next()).await {
                Ok(Some(Ok(()))) | Ok(None) => {}
                Ok(Some(Err(error))) => {
                    tracing::error!(%error, "HTTP task failed during shutdown");
                    cleanup = Err(failure(error));
                }
                Err(error) => {
                    connections.abort_all();
                    while let Some(joined) = connections.join_next().await {
                        if let Err(error) = joined {
                            tracing::debug!(%error, "HTTP task cancelled during shutdown");
                        }
                    }
                    cleanup = Err(failure(error));
                }
            }
        }
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; HTTP cleanup failed: {cleanup}"
            ))),
        }
    }
}
