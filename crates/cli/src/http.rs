use crate::{Error, Operation, RemoteOptions};
use reqwest::{Method, StatusCode};
use serde::{de::DeserializeOwned, Deserialize};
use std::time::Duration;
use url::{Host, Url};

const MAX_RESPONSE: usize = 1024 * 1024;

fn origin(address: &str) -> Result<Url, Error> {
    let url =
        Url::parse(address).map_err(|_| Error::ConnectionSettings("Invalid server address"))?;
    let loopback = match url.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(_)) | None => false,
    };
    if address.len() > 2048
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::ConnectionSettings(
            "Use an HTTPS origin or loopback HTTP origin",
        ));
    }
    Ok(url)
}

async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err(Error::Response("Server response exceeds capacity"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_RESPONSE - bytes.len() {
            return Err(Error::Response("Server response exceeds capacity"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Deserialize)]
struct Problem {
    message: String,
}

#[derive(Deserialize)]
struct Ping {
    ping: String,
    timestamp: String,
    asterisk_id: String,
}

pub(super) struct Remote<'a> {
    origin: Url,
    client: reqwest::Client,
    username: &'a str,
    password: String,
}

impl<'a> Remote<'a> {
    pub(super) fn new(options: &'a RemoteOptions, password: String) -> Result<Self, Error> {
        let origin = origin(&options.server)?;
        if options.username.is_empty()
            || options.username.len() > 128
            || options.username.contains(':')
        {
            return Err(Error::ConnectionSettings("Invalid username"));
        }
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5));
        let client = if origin.scheme() == "http" {
            builder.no_proxy()
        } else {
            builder
        }
        .build()?;
        Ok(Self {
            origin,
            client,
            username: &options.username,
            password,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        expected: StatusCode,
        timeout: Duration,
    ) -> Result<reqwest::Response, Error> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| Error::ConnectionSettings("Invalid server origin"))?
            .clear()
            .extend(path);
        let response = self
            .client
            .request(method, url)
            .basic_auth(self.username, Some(&self.password))
            .timeout(timeout)
            .send()
            .await?;
        let status = response.status();
        if status != expected {
            let message = serde_json::from_slice::<Problem>(&body(response).await?)
                .ok()
                .map(|problem| problem.message)
                .filter(|message| {
                    !message.is_empty()
                        && !message.contains(&self.password)
                        && !message.chars().any(char::is_control)
                })
                .unwrap_or_else(|| "Unexpected server response".to_owned());
            return Err(Error::Remote(status, message));
        }
        Ok(response)
    }

    pub(super) async fn get<T: DeserializeOwned>(&self, path: &[&str]) -> Result<T, Error> {
        let response = self
            .request(Method::GET, path, StatusCode::OK, Duration::from_secs(10))
            .await?;
        serde_json::from_slice(&body(response).await?)
            .map_err(|_| Error::Response("Invalid ARI response"))
    }

    pub(super) async fn control(&self, operation: Operation) -> Result<&'static str, Error> {
        match operation {
            Operation::Reload => {
                self.request(
                    Method::POST,
                    &["rusterisk", "configuration", "reload"],
                    StatusCode::NO_CONTENT,
                    Duration::from_secs(35),
                )
                .await?;
                Ok("Configuration reloaded.")
            }
            Operation::Ping => {
                let ping: Ping = self.get(&["ari", "asterisk", "ping"]).await?;
                if ping.ping != "pong" || ping.timestamp.is_empty() || ping.asterisk_id.is_empty() {
                    return Err(Error::Response("Invalid ARI ping response"));
                }
                Ok("pong")
            }
        }
    }
}
