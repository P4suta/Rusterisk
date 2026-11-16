use std::{io, net::SocketAddr};
use xtask::server_distribution::ServerRuntimeConfig;

fn sample(path: &str) -> io::Result<Vec<u8>> {
    Ok(match path {
        "config/http.conf" => include_bytes!("../../config/http.conf").to_vec(),
        "config/ari.conf" => include_bytes!("../../config/ari.conf").to_vec(),
        "config/pjsip.conf" => include_bytes!("../../config/pjsip.conf").to_vec(),
        "config/rtp.conf" => include_bytes!("../../config/rtp.conf").to_vec(),
        "config/extensions.conf" => include_bytes!("../../config/extensions.conf").to_vec(),
        _ => return Err(io::Error::other("unexpected sample input")),
    })
}

#[test]
fn runtime_configuration_changes_only_the_two_sample_listener_ports() -> io::Result<()> {
    let configuration = ServerRuntimeConfig::capture(
        "127.0.0.1:18088"
            .parse::<SocketAddr>()
            .map_err(io::Error::other)?,
        "127.0.0.1:15060"
            .parse::<SocketAddr>()
            .map_err(io::Error::other)?,
        sample,
    )?;
    let files: Vec<_> = configuration.files().collect();
    assert_eq!(files.len(), 5);
    for (path, bytes) in files {
        let original = String::from_utf8(sample(path)?).map_err(io::Error::other)?;
        let expected = match path {
            "config/http.conf" => original.replace("bindport=8088\n", "bindport=18088\n"),
            "config/pjsip.conf" => {
                original.replace("bind=127.0.0.1:5060\n", "bind=127.0.0.1:15060\n")
            }
            _ => original,
        };
        assert_eq!(bytes, expected.as_bytes());
    }
    Ok(())
}

#[test]
fn runtime_configuration_rejects_unbound_public_and_changed_samples() -> io::Result<()> {
    let http: SocketAddr = "127.0.0.1:18088".parse().map_err(io::Error::other)?;
    let sip: SocketAddr = "127.0.0.1:15060".parse().map_err(io::Error::other)?;
    for address in [
        "0.0.0.0:18088",
        "127.0.0.2:18088",
        "127.0.0.1:0",
        "[::1]:18088",
    ] {
        let address = address.parse().map_err(io::Error::other)?;
        assert!(ServerRuntimeConfig::capture(address, sip, sample).is_err());
        assert!(ServerRuntimeConfig::capture(http, address, sample).is_err());
    }
    for variant in 0..4 {
        assert!(ServerRuntimeConfig::capture(http, sip, |path| {
            let mut bytes = sample(path)?;
            if path == "config/http.conf" {
                bytes = match variant {
                    0 => b"[general]\nbindport=9000\n".to_vec(),
                    1 => [bytes.as_slice(), b"bindport=8088\n"].concat(),
                    2 => [0xff].to_vec(),
                    _ => vec![b'x'; 65_537],
                };
            }
            Ok(bytes)
        })
        .is_err());
    }
    Ok(())
}
