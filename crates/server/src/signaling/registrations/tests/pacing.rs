use super::config;
use crate::{media::MediaHub, signaling::Signaling, ControlError, Controller};
use std::{future::Future, net::UdpSocket, time::Duration};
use tokio::sync::watch;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn registrar() -> Result<UdpSocket, ControlError> {
    let socket = UdpSocket::bind("127.0.0.1:0").map_err(failure)?;
    socket.set_nonblocking(true).map_err(failure)?;
    Ok(socket)
}

fn requests(registrars: &[UdpSocket]) -> usize {
    let mut bytes = [0; 2048];
    registrars
        .iter()
        .filter(|registrar| {
            registrar
                .recv_from(&mut bytes)
                .is_ok_and(|(size, _)| bytes[..size].starts_with(b"REGISTER "))
        })
        .count()
}

#[tokio::test(start_paused = true)]
async fn registrations_that_fall_due_together_leave_one_interval_apart() -> Result<(), ControlError>
{
    let peers = ["127.0.0.1:19001", "127.0.0.1:19002", "127.0.0.1:19003"];
    let config = config(&peers, 0)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    tokio::time::resume();
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let mut sent = signaling.network.capture_datagrams()?;
    let (_stopper, mut stop) = watch::channel(false);
    tokio::time::pause();
    let mut serving = std::pin::pin!(signaling.serve(&mut stop, controller.clock()));
    let mut arrivals = Vec::with_capacity(peers.len());
    let mut destinations = std::collections::BTreeSet::new();
    for _ in &peers {
        let (at, packet) = tokio::select! {
            packet = sent.recv() => packet.ok_or(ControlError::Capacity)?,
            stopped = &mut serving => return Err(failure(format!("signaling stopped: {stopped:?}"))),
            () = tokio::time::sleep(Duration::from_secs(3)) => return Err(failure("registration deadline expired")),
        };
        assert!(packet.bytes.starts_with(b"REGISTER "));
        destinations.insert(packet.peer.to_string());
        arrivals.push(at);
    }
    assert_eq!(destinations.into_iter().collect::<Vec<_>>(), peers);
    for pair in arrivals.windows(2) {
        assert_eq!(pair[1] - pair[0], Duration::from_millis(20));
    }
    Ok(())
}

#[tokio::test]
async fn each_registrar_receives_its_request_on_a_live_datagram_socket() -> Result<(), ControlError>
{
    let registrars = [registrar()?, registrar()?, registrar()?];
    let peers = registrars
        .iter()
        .map(|socket| socket.local_addr().map(|peer| peer.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    let config = config(&peers.iter().map(String::as_str).collect::<Vec<_>>(), 0)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let (_stopper, mut stop) = watch::channel(false);
    let receiving = async {
        for registrar in registrars {
            let socket = tokio::net::UdpSocket::from_std(registrar).map_err(failure)?;
            let mut bytes = [0; 2048];
            let (size, _) = socket.recv_from(&mut bytes).await.map_err(failure)?;
            assert!(bytes[..size].starts_with(b"REGISTER "));
        }
        Ok(())
    };
    tokio::select! {
        result = rusterisk_test_support::Wait::Ready.bounded(receiving) => result.map_err(failure)?,
        stopped = signaling.serve(&mut stop, controller.clock()) => Err(failure(format!("signaling stopped: {stopped:?}"))),
    }
}

#[tokio::test(start_paused = true)]
async fn a_signaling_task_sleeps_until_a_registration_falls_due() -> Result<(), ControlError> {
    let registrar = registrar()?;
    let peer = registrar.local_addr().map_err(failure)?.to_string();
    let config = config(&[peer.as_str()], 0)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let (_stopper, mut stop) = watch::channel(false);
    let mut polls = 0_u32;
    {
        let mut serving = std::pin::pin!(signaling.serve(&mut stop, controller.clock()));
        let counted = std::future::poll_fn(|context| {
            polls += 1;
            serving.as_mut().poll(context)
        });
        tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_millis(999)) => {}
            served = counted => return Err(failure(format!("the signaling task stopped: {served:?}"))),
        }
    }
    assert_eq!(
        polls, 1,
        "the signaling task woke before its registration fell due"
    );
    assert_eq!(requests(std::slice::from_ref(&registrar)), 0);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_signaling_task_without_pending_work_takes_no_turn_for_an_hour(
) -> Result<(), ControlError> {
    let config = config(&[], 0)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let (_stopper, mut stop) = watch::channel(false);
    let mut polls = 0_u32;
    {
        let mut serving = std::pin::pin!(signaling.serve(&mut stop, controller.clock()));
        let counted = std::future::poll_fn(|context| {
            polls += 1;
            serving.as_mut().poll(context)
        });
        tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_secs(3600)) => {}
            served = counted => return Err(failure(format!("the signaling task stopped: {served:?}"))),
        }
    }
    assert_eq!(polls, 1, "an idle signaling task woke without an event");
    Ok(())
}
