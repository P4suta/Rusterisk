use rusterisk_server::rtp::SocketPair;
use std::net::{IpAddr, Ipv4Addr};

#[tokio::test]
async fn paired_ports_are_exclusive_and_released_with_their_owner() -> Result<(), std::io::Error> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let pair = SocketPair::bind(ip, 0, 0)?;
    let address = pair.local_addr()?;
    assert_eq!(address.port() % 2, 0);
    assert_eq!(pair.control_addr()?.port(), address.port() + 1);
    assert!(SocketPair::bind(ip, address.port(), address.port() + 1).is_err());
    drop(pair);
    let replacement = SocketPair::bind(ip, address.port(), address.port() + 1)?;
    assert_eq!(replacement.local_addr()?, address);
    assert!(SocketPair::bind(ip, 10001, 10002).is_err());
    assert!(SocketPair::bind(ip, 20000, 10000).is_err());
    Ok(())
}
