use std::{io, net::SocketAddr};
use xtask::server_distribution::check_server_ready_log;

#[test]
fn only_complete_ready_records_for_the_owned_listener_count() -> io::Result<()> {
    let expected: SocketAddr = "127.0.0.1:18088".parse().map_err(io::Error::other)?;
    let record = b"{\"fields\":{\"message\":\"server ready\",\"http\":\"127.0.0.1:18088\"}}\n";
    for end in 0..record.len() {
        assert!(!check_server_ready_log(&record[..end], expected)?);
    }
    assert!(check_server_ready_log(record, expected)?);
    let other = b"{\"fields\":{\"message\":\"starting\"}}\n";
    assert!(check_server_ready_log(
        &[other.as_slice(), record].concat(),
        expected
    )?);
    assert!(check_server_ready_log(
        &[record.as_slice(), b"{\"partial"].concat(),
        expected
    )?);
    Ok(())
}

#[test]
fn invalid_duplicate_or_unowned_ready_records_are_rejected() -> io::Result<()> {
    let expected: SocketAddr = "127.0.0.1:18088".parse().map_err(io::Error::other)?;
    for record in [
        "broken\n",
        "{\"fields\":{\"message\":\"server ready\"}}\n",
        "{\"fields\":{\"message\":\"server ready\",\"http\":\"127.0.0.1:18089\"}}\n",
        "{\"fields\":{\"message\":\"server ready\",\"http\":\"0.0.0.0:18088\"}}\n",
    ] {
        assert!(check_server_ready_log(record.as_bytes(), expected).is_err());
    }
    let record = b"{\"fields\":{\"message\":\"server ready\",\"http\":\"127.0.0.1:18088\"}}\n";
    assert!(check_server_ready_log(&[record.as_slice(), record].concat(), expected).is_err());
    assert!(check_server_ready_log(&vec![b' '; 1024 * 1024 + 1], expected).is_err());
    Ok(())
}
