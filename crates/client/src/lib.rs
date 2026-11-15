use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
pub use rusterisk_api as api;
use serde::de::DeserializeOwned;
use std::time::Duration;

const MAX_RESPONSE: usize = 1024 * 1024;
mod session;
pub use session::Session;
mod owned_call;
pub use owned_call::{CallFailure, CallSnapshot, Cleanup, Originate, OwnedCall};
pub use rusterisk_core::{CallCompletion, CallMilestone, OwnedCallProgress};
pub use rusterisk_core::{DtmfDigit, DtmfTiming, MuteDirection};
mod controls;
mod recordings;

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Id(api::InvalidId),
    Io(std::io::Error),
    Transport(reqwest::Error),
    Json(serde_json::Error),
    Api { status: u16, message: api::ApiError },
    Capacity,
    Closed,
    Timeout,
    WebSocket(String),
    EventLagged(u64),
    CallEnded(CallCompletion),
    CallFailed(CallFailure),
    Cleanup(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Id(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::Api { status, message } => write!(f, "ARI {status}: {}", message.message),
            Self::Capacity => f.write_str("client capacity exceeded"),
            Self::Closed => f.write_str("client session closed"),
            Self::Timeout => f.write_str("client deadline expired"),
            Self::WebSocket(message) => write!(f, "event transport: {message}"),
            Self::EventLagged(count) => write!(f, "event consumer missed {count} events"),
            Self::CallEnded(completion) => write!(f, "call completed: {completion:?}"),
            Self::CallFailed(failure) => f.write_str(&failure.message),
            Self::Cleanup(message) => write!(f, "call cleanup: {message}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<api::InvalidId> for Error {
    fn from(error: api::InvalidId) -> Self {
        Self::Id(error)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(error.to_string())
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: url::Url,
    authorization: HeaderValue,
}

impl Client {
    pub fn new(address: &str, username: &str, password: &str) -> Result<Self, Error> {
        if address.len() > 2048 {
            return Err(Error::Invalid("server address exceeds 2048 bytes"));
        }
        let base =
            url::Url::parse(address).map_err(|_| Error::Invalid("invalid server address"))?;
        let loopback = match base.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(_)) | None => false,
        };
        if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || base.host().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::Invalid(
                "use an HTTPS origin or loopback HTTP origin",
            ));
        }
        if username.is_empty()
            || username.len() > 128
            || username.contains(':')
            || password.len() > 1024
        {
            return Err(Error::Invalid("invalid client credentials"));
        }
        let mut authorization = HeaderValue::from_str(&format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        ))
        .map_err(|_| Error::Invalid("invalid client credentials"))?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization.clone());
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(4)
            .build()?;
        Ok(Self {
            http,
            base,
            authorization,
        })
    }

    pub async fn subscribe(&self, application: &api::ChannelId) -> Result<Session, Error> {
        session::connect(self, application).await
    }

    fn prepare_request(
        &self,
        operation: &str,
        path: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Result<reqwest::RequestBuilder, Error> {
        if query.len() > 32
            || query
                .iter()
                .any(|(name, value)| name.len() > 128 || value.len() > 4096)
        {
            return Err(Error::Capacity);
        }
        let operation = api::OPERATIONS
            .iter()
            .find(|entry| entry.id == operation)
            .ok_or(Error::Invalid("unknown operation"))?;
        let mut resource = operation.path.to_owned();
        for (key, value) in path {
            api::ChannelId::try_from((*value).to_owned())?;
            resource = resource.replace(&format!("{{{key}}}"), value);
        }
        if resource.contains(['{', '}']) {
            return Err(Error::Invalid("missing resource identifier"));
        }
        let mut url = self.base.clone();
        url.set_path(&format!("/ari{resource}"));
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        if url.as_str().len() > 4096 {
            return Err(Error::Capacity);
        }
        let method = reqwest::Method::from_bytes(operation.method.upper().as_bytes())
            .map_err(|_| Error::Invalid("invalid operation method"))?;
        Ok(self.http.request(method, url))
    }

    async fn request<T: DeserializeOwned>(
        &self,
        operation: &str,
        path: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Result<T, Error> {
        Self::decode_response(self.prepare_request(operation, path, query)?.send().await?).await
    }

    async fn decode_response<T: DeserializeOwned>(
        mut response: reqwest::Response,
    ) -> Result<T, Error> {
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE as u64)
        {
            return Err(Error::Capacity);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_RESPONSE {
                return Err(Error::Capacity);
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                message: serde_json::from_slice(&body)?,
            });
        }
        Ok(serde_json::from_slice(if body.is_empty() {
            b"null"
        } else {
            &body
        })?)
    }

    pub async fn ping(&self) -> Result<api::AsteriskPing, Error> {
        self.request("asterisk.ping", &[], &[]).await
    }
    pub async fn channels(&self) -> Result<Vec<api::Channel>, Error> {
        self.request("channels.list", &[], &[]).await
    }
    pub async fn channel(&self, id: &api::ChannelId) -> Result<api::Channel, Error> {
        self.request("channels.get", &[("channelId", id.as_str())], &[])
            .await
    }
    pub async fn answer(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.answer", &[("channelId", id.as_str())], &[])
            .await
    }
    pub async fn hangup(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.hangup", &[("channelId", id.as_str())], &[])
            .await
    }
    pub async fn bridges(&self) -> Result<Vec<api::Bridge>, Error> {
        self.request("bridges.list", &[], &[]).await
    }
    pub async fn bridge(&self, id: &api::ChannelId) -> Result<api::Bridge, Error> {
        self.request("bridges.get", &[("bridgeId", id.as_str())], &[])
            .await
    }
    pub async fn create_bridge(
        &self,
        id: &api::ChannelId,
        name: &str,
    ) -> Result<api::Bridge, Error> {
        self.request(
            "bridges.createWithId",
            &[("bridgeId", id.as_str())],
            &[("name", name)],
        )
        .await
    }
    pub async fn destroy_bridge(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("bridges.destroy", &[("bridgeId", id.as_str())], &[])
            .await
    }
}
