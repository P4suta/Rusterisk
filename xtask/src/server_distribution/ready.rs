use serde::Deserialize;
use std::{io, net::SocketAddr};

#[derive(Deserialize)]
struct Record {
    fields: Fields,
}

#[derive(Deserialize)]
struct Fields {
    message: Option<String>,
    http: Option<String>,
}

pub fn check_server_ready_log(bytes: &[u8], expected: SocketAddr) -> io::Result<bool> {
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::other(
            "server startup log exceeded its byte limit",
        ));
    }
    let mut ready = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.last() != Some(&b'\n') {
            break;
        }
        let record: Record = serde_json::from_slice(line).map_err(io::Error::other)?;
        if record.fields.message.as_deref() != Some("server ready") {
            continue;
        }
        let address = record
            .fields
            .http
            .as_deref()
            .and_then(|address| address.parse::<SocketAddr>().ok())
            .ok_or_else(|| io::Error::other("server ready record lacks a bound listener"))?;
        if ready || address != expected || !address.ip().is_loopback() || address.port() == 0 {
            return Err(io::Error::other(
                "server ready record changed the owned listener",
            ));
        }
        ready = true;
    }
    Ok(ready)
}
