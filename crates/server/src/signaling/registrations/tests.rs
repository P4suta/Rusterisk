use super::{Registrations, State};
use crate::ControlError;
use rusterisk_config::{Config, MAX_REGISTRATIONS};
mod dns;
mod generations;
mod reuse;
mod reuse_capacity;
mod stream;
mod stream_deadlines;
mod stream_lifetime;
mod tls;

fn config(peers: &[&str], delay: u32) -> Result<Config, ControlError> {
    let mut pjsip = "[native]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[account]\ntype=auth\nusername=account\npassword=sample\n".to_owned();
    for (index, peer) in peers.iter().enumerate() {
        pjsip.push_str(&format!("[provider-{index}]\ntype=registration\ntransport=native\noutbound_auth=account\nserver_uri=sip:{peer}\nclient_uri=sip:account{index}@provider.example\nmax_random_initial_delay={delay}\n"));
    }
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &pjsip,
        "",
        "",
    )
    .map_err(super::failure)
}

#[test]
fn initial_delay_follows_the_reference_one_second_floor_and_configured_maximum(
) -> Result<(), ControlError> {
    for delay in [0, 1, 10, u32::MAX] {
        let config = config(&["127.0.0.2:5070"], delay)?;
        let manager = Registrations::new(&config)?;
        let Some(client) = manager.clients.first() else {
            return Err(ControlError::Capacity);
        };
        let (State::Waiting { at } | State::Ready { at, .. }) = client.state else {
            return Err(ControlError::Invalid("Expected initial registration delay"));
        };
        assert!((1000..=u64::from(delay.max(1)) * 1000).contains(&at));
    }
    Ok(())
}

#[test]
fn the_manager_capacity_is_checked_before_allocating_client_work() -> Result<(), ControlError> {
    let peers = ["127.0.0.2:5070"; MAX_REGISTRATIONS];
    let mut config = config(&peers, 0)?;
    assert_eq!(
        Registrations::new(&config)?.clients.len(),
        MAX_REGISTRATIONS
    );
    let mut extra = config
        .registrations
        .first()
        .ok_or(ControlError::Capacity)?
        .clone();
    extra.name = "overflow".into();
    config.registrations.push(extra);
    assert!(matches!(
        Registrations::new(&config),
        Err(ControlError::Capacity)
    ));
    Ok(())
}

#[test]
fn round_robin_preserves_same_registrar_serialization_until_transaction_settlement(
) -> Result<(), ControlError> {
    let config = config(&["127.0.0.2:5070", "127.0.0.2:5070", "127.0.0.3:5070"], 0)?;
    let mut manager = Registrations::new(&config)?;
    let local = "127.0.0.1:5068".parse().map_err(super::failure)?;
    assert_eq!(manager.select(1000), Some(0));
    assert!(manager.poll(0, local, 1000)?.is_some());
    assert_eq!(manager.select(1000), Some(2));
    assert!(manager.poll(2, local, 1000)?.is_some());
    assert_eq!(manager.select(1000), None);
    assert_eq!(manager.select(33_000), Some(0));
    assert!(manager.poll(0, local, 33_000)?.is_none());
    assert_eq!(manager.select(33_000), Some(1));
    assert!(manager.poll(1, local, 33_000)?.is_some());
    Ok(())
}

#[test]
fn cancellation_stops_unsent_and_transmitted_registration_clients() -> Result<(), ControlError> {
    let config = config(&["127.0.0.2:5070", "127.0.0.3:5070"], 0)?;
    let mut manager = Registrations::new(&config)?;
    assert_eq!(manager.select(1000), Some(0));
    assert!(manager
        .poll(0, "127.0.0.1:5068".parse().map_err(super::failure)?, 1000)?
        .is_some());
    manager.cancel();
    assert_eq!(manager.select(u64::MAX), None);
    assert!(manager
        .clients
        .iter()
        .all(|client| matches!(client.state, State::Stopped)));
    Ok(())
}
