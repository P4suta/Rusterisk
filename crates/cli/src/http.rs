use crate::{Error, Operation, RemoteOptions};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
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

pub(super) async fn request(
    operation: Operation,
    options: &RemoteOptions,
    password: &str,
) -> Result<&'static str, Error> {
    let mut url = origin(&options.server)?;
    if options.username.is_empty() || options.username.len() > 128 || options.username.contains(':')
    {
        return Err(Error::ConnectionSettings("Invalid username"));
    }
    let (method, path, timeout, expected) = match operation {
        Operation::Ping => (Method::GET, "/ari/asterisk/ping", 10, StatusCode::OK),
        Operation::Reload => (
            Method::POST,
            "/rusterisk/configuration/reload",
            35,
            StatusCode::NO_CONTENT,
        ),
    };
    url.set_path(path);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(timeout))
        .build()?;
    let response = client
        .request(method, url)
        .basic_auth(&options.username, Some(password))
        .send()
        .await?;
    let status = response.status();
    if status != expected {
        let message = serde_json::from_slice::<Problem>(&body(response).await?)
            .ok()
            .map(|problem| problem.message)
            .filter(|message| {
                !message.is_empty()
                    && !message.contains(password)
                    && !message.chars().any(char::is_control)
            })
            .unwrap_or_else(|| "Unexpected server response".to_owned());
        return Err(Error::Remote(status, message));
    }
    match operation {
        Operation::Reload => Ok("Configuration reloaded."),
        Operation::Ping => {
            let ping: Ping = serde_json::from_slice(&body(response).await?)
                .map_err(|_| Error::Response("Invalid ARI ping response"))?;
            if ping.ping != "pong" || ping.timestamp.is_empty() || ping.asterisk_id.is_empty() {
                return Err(Error::Response("Invalid ARI ping response"));
            }
            Ok("pong")
        }
    }
}
