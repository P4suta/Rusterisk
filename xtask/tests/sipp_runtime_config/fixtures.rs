use rusterisk_config::Config;
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
};
use xtask::server_distribution::ServerRuntimeConfig;

fn read(path: &str) -> io::Result<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace missing"))?;
    std::fs::read(root.join(path))
}

#[test]
fn all_generated_registration_settings_pass_the_native_configuration_parser() -> io::Result<()> {
    let http = SocketAddr::from((Ipv4Addr::LOCALHOST, 49152));
    let runtime = ServerRuntimeConfig::for_registration_comparison(http, 49153, read)?;
    let source = |name: &str| -> io::Result<&str> {
        let mut files = runtime.files().filter(|(path, _)| *path == name);
        let (_, bytes) = files
            .next()
            .ok_or_else(|| io::Error::other(format!("Generated {name} is missing")))?;
        if files.next().is_some() {
            return Err(io::Error::other(format!("Generated {name} is ambiguous")));
        }
        std::str::from_utf8(bytes).map_err(io::Error::other)
    };
    let parsed = Config::parse(
        source("config/http.conf")?,
        source("config/ari.conf")?,
        source("config/pjsip.conf")?,
        source("config/rtp.conf")?,
        source("config/extensions.conf")?,
    )
    .map_err(io::Error::other)?;
    assert_eq!(parsed.http.bind, http);
    let [transport] = parsed.transports.as_slice() else {
        return Err(io::Error::other(
            "The comparison SIP listener is not unique",
        ));
    };
    assert_eq!(
        transport.bind,
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 49153))
    );
    assert!(parsed
        .endpoints
        .iter()
        .any(|endpoint| endpoint.name == "phone" && endpoint.transport == transport.name));
    Ok(())
}

#[test]
fn registration_comparison_preserves_reference_identities_and_selected_listener_values(
) -> io::Result<()> {
    let http = SocketAddr::from((Ipv4Addr::LOCALHOST, 49152));
    let config = ServerRuntimeConfig::for_registration_comparison(http, 49153, read)?;
    let files: Vec<_> = config.files().collect();
    assert_eq!(files.len(), 5);
    for name in ["http", "ari", "pjsip", "rtp", "extensions"] {
        let original = read(&format!("tests/interop/asterisk/{name}.conf"))?;
        let expected = match name {
            "http" => String::from_utf8(original)
                .map_err(io::Error::other)?
                .replace("bindaddr = 0.0.0.0\n", "bindaddr = 127.0.0.1\n")
                .replace("bindport = 8088\n", "bindport = 49152\n")
                .into_bytes(),
            "pjsip" => String::from_utf8(original)
                .map_err(io::Error::other)?
                .replace("bind = 0.0.0.0:5060\n", "bind = 0.0.0.0:49153\n")
                .into_bytes(),
            "ari" | "rtp" | "extensions" => original,
            _ => return Err(io::Error::other("unexpected reference fixture")),
        };
        let path = format!("config/{name}.conf");
        let (_, actual) = files
            .iter()
            .find(|(candidate, _)| *candidate == path)
            .ok_or_else(|| io::Error::other("comparison configuration file missing"))?;
        assert_eq!(*actual, expected);
    }
    Ok(())
}

#[test]
fn comparison_configuration_rejects_external_http_and_missing_ports_before_reading() {
    for http in [
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 49152)),
        SocketAddr::from((Ipv4Addr::new(203, 0, 113, 1), 49152)),
        SocketAddr::from((Ipv4Addr::new(127, 0, 0, 2), 49152)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, 49152)),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    ] {
        let mut reads = 0;
        assert!(
            ServerRuntimeConfig::for_registration_comparison(http, 49153, |_| {
                reads += 1;
                Err(io::Error::other("unexpected read"))
            })
            .is_err()
        );
        assert_eq!(reads, 0);
    }
    let mut reads = 0;
    assert!(ServerRuntimeConfig::for_registration_comparison(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 49152)),
        0,
        |_| {
            reads += 1;
            Err(io::Error::other("unexpected read"))
        }
    )
    .is_err());
    assert_eq!(reads, 0);
}

#[test]
fn changed_ambiguous_and_oversize_reference_profiles_are_rejected() {
    let http = SocketAddr::from((Ipv4Addr::LOCALHOST, 49152));
    for changed in [
        Vec::new(),
        [0xff, 0xfe].to_vec(),
        vec![b'x'; 65_537],
        b"[general]\nbindaddr = 127.0.0.1\nbindport = 8088\n".to_vec(),
        b"[general]\nbindaddr = 0.0.0.0\nbindport = 8088\nbindport = 8088\n".to_vec(),
    ] {
        assert!(
            ServerRuntimeConfig::for_registration_comparison(http, 49153, |path| {
                if path == "tests/interop/asterisk/http.conf" {
                    Ok(changed.clone())
                } else {
                    read(path)
                }
            })
            .is_err()
        );
    }
    assert!(
        ServerRuntimeConfig::for_registration_comparison(http, 49153, |path| {
            let bytes = read(path)?;
            if path == "tests/interop/asterisk/pjsip.conf" {
                Ok(String::from_utf8(bytes)
                    .map_err(io::Error::other)?
                    .replace("bind = 0.0.0.0:5060\n", "bind = 127.0.0.1:5060\n")
                    .into_bytes())
            } else {
                Ok(bytes)
            }
        })
        .is_err()
    );
}
