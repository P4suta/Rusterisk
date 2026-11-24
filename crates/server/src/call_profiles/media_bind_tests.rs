use super::NativeMediaProfile;
use crate::ControlError;
use rusterisk_config::Config;
use std::net::{IpAddr, Ipv4Addr};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const LAN: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
const INTERFACE: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20));

struct Arrangement {
    transport: &'static str,
    external: Option<IpAddr>,
    webrtc: bool,
}

const BROWSER_SOCKET: &str = "protocol=ws\nbind=127.0.0.1\n";
const LOOPBACK_DATAGRAMS: &str = "protocol=udp\nbind=127.0.0.1:5060\n";
const INTERFACE_DATAGRAMS: &str = "protocol=udp\nbind=192.0.2.20:5060\n";

fn bind_ip(arrangement: &Arrangement) -> Result<IpAddr, ControlError> {
    let external = arrangement
        .external
        .map(|address| format!("external_media_address={address}\n"))
        .unwrap_or_default();
    let webrtc = if arrangement.webrtc {
        "webrtc=yes\n"
    } else {
        ""
    };
    let sip = format!(
        "[signaling]\ntype=transport\n{}{external}[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone-endpoint]\ntype=endpoint\ntransport=signaling\nauth=auth\naors=phone\nallow=ulaw\n{webrtc}dtmf_mode=rfc4733\ncontext=internal\n",
        arrangement.transport,
    );
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Stasis(calls)\n",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    Ok(NativeMediaProfile::requested(&config, "phone-endpoint")?.bind_ip())
}

#[test]
fn a_browser_endpoint_behind_loopback_signaling_binds_media_to_the_external_address(
) -> Result<(), ControlError> {
    let address = bind_ip(&Arrangement {
        transport: BROWSER_SOCKET,
        external: Some(LAN),
        webrtc: true,
    })?;
    assert_eq!(address, LAN);
    Ok(())
}

#[test]
fn a_browser_endpoint_without_an_external_address_keeps_the_signaling_address(
) -> Result<(), ControlError> {
    let address = bind_ip(&Arrangement {
        transport: BROWSER_SOCKET,
        external: None,
        webrtc: true,
    })?;
    assert_eq!(address, LOOPBACK);
    Ok(())
}

#[test]
fn a_packet_audio_endpoint_behind_loopback_signaling_keeps_the_signaling_address(
) -> Result<(), ControlError> {
    let address = bind_ip(&Arrangement {
        transport: LOOPBACK_DATAGRAMS,
        external: Some(LAN),
        webrtc: false,
    })?;
    assert_eq!(address, LOOPBACK);
    Ok(())
}

#[test]
fn a_browser_endpoint_on_an_interface_address_keeps_the_signaling_address(
) -> Result<(), ControlError> {
    let address = bind_ip(&Arrangement {
        transport: INTERFACE_DATAGRAMS,
        external: Some(LAN),
        webrtc: true,
    })?;
    assert_eq!(address, INTERFACE);
    Ok(())
}
