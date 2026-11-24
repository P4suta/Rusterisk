use super::config;
use crate::{media::MediaHub, signaling::Signaling, ControlError, Controller};
use std::{future::Future, net::UdpSocket, pin::Pin, time::Duration};
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

async fn run<F>(serving: &mut Pin<&mut F>, duration: Duration) -> Result<(), ControlError>
where
    F: Future<Output = Result<(), ControlError>>,
{
    tokio::select! {
        biased;
        () = tokio::time::sleep(duration) => Ok(()),
        served = serving => Err(failure(format!("the signaling task stopped: {served:?}"))),
    }
}

#[tokio::test(start_paused = true)]
async fn registrations_that_fall_due_together_leave_one_interval_apart() -> Result<(), ControlError>
{
    let registrars = [registrar()?, registrar()?, registrar()?];
    let peers = registrars
        .iter()
        .map(|registrar| registrar.local_addr().map(|peer| peer.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    let peers = peers.iter().map(String::as_str).collect::<Vec<_>>();
    let config = config(&peers, 0)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let (_stopper, mut stop) = watch::channel(false);
    let mut serving = std::pin::pin!(signaling.serve(&mut stop, controller.clock()));
    run(&mut serving, Duration::from_millis(999)).await?;
    assert_eq!(requests(&registrars), 0);
    let mut arrivals = Vec::new();
    for instant in 1000_u64..1400 {
        run(&mut serving, Duration::from_millis(1)).await?;
        let arrived = requests(&registrars);
        assert!(arrived <= 1, "{arrived} registrations left at {instant} ms");
        arrivals.extend(std::iter::repeat_n(instant, arrived));
    }
    assert_eq!(arrivals.len(), 3, "registrations left at {arrivals:?}");
    assert!(
        arrivals.windows(2).all(|pair| pair[1] - pair[0] >= 20),
        "registrations left at {arrivals:?}"
    );
    Ok(())
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
