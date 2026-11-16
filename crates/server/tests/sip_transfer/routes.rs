use super::RequestRoute;
use std::net::SocketAddr;

#[test]
fn direct_requests_keep_the_exact_server_peer() {
    let server = SocketAddr::from(([127, 0, 0, 1], 5060));
    let relay = SocketAddr::from(([127, 0, 0, 1], 49000));
    let mut route = RequestRoute::Direct(server);
    assert!(!route.allows(relay));
    assert!(route.allows(server));
    assert!(!route.allows(SocketAddr::from(([192, 168, 215, 2], 5060))));
}

#[test]
fn callback_requests_pin_one_server_or_loopback_relay_and_reject_replacements() {
    let server = SocketAddr::from(([192, 168, 215, 2], 5060));
    let relay = SocketAddr::from(([127, 0, 0, 1], 49000));
    let mut route = RequestRoute::CallbackPending { server };
    for rejected in [
        SocketAddr::from(([192, 168, 215, 3], 5060)),
        SocketAddr::from(([192, 168, 215, 2], 5061)),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SocketAddr::from(([224, 0, 0, 1], 5060)),
    ] {
        assert!(!route.allows(rejected));
    }
    assert!(route.allows(relay));
    assert!(route.allows(relay));
    assert!(!route.allows(server));
    assert!(!route.allows(SocketAddr::from(([127, 0, 0, 1], 49001))));
    let mut route = RequestRoute::CallbackPending { server };
    assert!(route.allows(server));
    assert!(!route.allows(relay));
}
