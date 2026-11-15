use rusterisk_config::Protocol;
use std::net::SocketAddr;

pub(super) fn token(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Udp => "UDP",
        Protocol::Tcp => "TCP",
        Protocol::Tls => "TLS",
        Protocol::Ws => "WS",
        Protocol::Wss => "WSS",
    }
}

pub(super) fn contact(protocol: Protocol, local: SocketAddr) -> String {
    let parameter = match protocol {
        Protocol::Udp => "",
        Protocol::Tcp => ";transport=tcp",
        Protocol::Tls => ";transport=tls",
        Protocol::Ws => ";transport=ws",
        Protocol::Wss => ";transport=ws",
    };
    let scheme = if protocol == Protocol::Wss {
        "sips"
    } else {
        "sip"
    };
    format!("<{scheme}:rusterisk@{local}{parameter}>")
}
