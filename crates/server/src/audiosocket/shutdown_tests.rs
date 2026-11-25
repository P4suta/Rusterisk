use super::{failure, AudioSocketLink, Channels, LinkStats};
use crate::media_link::DigitCommand;
use rusterisk_core::{DtmfToken, DtmfTone, PeerMediaDirection};
use rusterisk_media::{AudioSocket, Frame, MediaError};
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};

struct Application {
    _incoming: mpsc::Receiver<Frame>,
    _outgoing: mpsc::Sender<Frame>,
    _digits: mpsc::Receiver<DtmfTone>,
    _commands: mpsc::Sender<DigitCommand>,
    _completed: mpsc::Receiver<DtmfToken>,
    stop: watch::Sender<bool>,
    _direction: watch::Sender<PeerMediaDirection>,
}

struct Pair {
    link: AudioSocketLink,
    peer: TcpStream,
    application: Application,
}

async fn bounded<T>(future: impl Future<Output = Result<T, MediaError>>) -> Result<T, MediaError> {
    tokio::time::timeout(Duration::from_secs(1), future)
        .await
        .map_err(failure)?
}

async fn pair() -> Result<Pair, MediaError> {
    let listener =
        bounded(async { TcpListener::bind("127.0.0.1:0").await.map_err(failure) }).await?;
    let target = listener.local_addr().map_err(failure)?;
    let (peer, (socket, _)) = bounded(async {
        tokio::try_join!(TcpStream::connect(target), listener.accept()).map_err(failure)
    })
    .await?;
    let (incoming, input) = mpsc::channel(3);
    let (outgoing, output) = mpsc::channel(3);
    let (digits, received_digits) = mpsc::channel(16);
    let (commands, command) = mpsc::channel(8);
    let (completed, completions) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
    Ok(Pair {
        link: AudioSocketLink::new(
            socket,
            Channels {
                incoming,
                outgoing: output,
                digits,
                commands: command,
                completed,
                stop: stopped,
                direction: directed,
            },
            8000,
        )?,
        peer,
        application: Application {
            _incoming: input,
            _outgoing: outgoing,
            _digits: received_digits,
            _commands: commands,
            _completed: completions,
            stop,
            _direction: direction,
        },
    })
}

fn observed<T>(
    result: Result<LinkStats, MediaError>,
    peer: Result<T, MediaError>,
) -> Result<(Result<LinkStats, MediaError>, T), MediaError> {
    match peer {
        Ok(peer) => Ok((result, peer)),
        Err(peer) => match result {
            Ok(_) => Err(peer),
            Err(primary) => Err(failure(format!(
                "{primary}; peer observation failed: {peer}"
            ))),
        },
    }
}

async fn termination(peer: &mut TcpStream) -> Result<(), MediaError> {
    let mut bytes = [0; 3];
    peer.read_exact(&mut bytes).await.map_err(failure)?;
    if bytes != AudioSocket::Terminate.encode()?.as_slice() {
        return Err(MediaError::InvalidPacket);
    }
    let mut extra = [0; 1];
    if peer.read(&mut extra).await.map_err(failure)? != 0 {
        return Err(MediaError::InvalidPacket);
    }
    Ok(())
}

#[tokio::test]
async fn observed_peer_eof_after_complete_frames_closes_without_a_termination_reply(
) -> Result<(), MediaError> {
    let Pair {
        link,
        mut peer,
        application: _application,
    } = pair().await?;
    bounded(async {
        peer.write_all(&AudioSocket::Dtmf(b'5').encode()?)
            .await
            .map_err(failure)?;
        peer.shutdown().await.map_err(failure)
    })
    .await?;
    let (result, reply) = tokio::join!(
        bounded(link.run()),
        bounded(async {
            let mut reply = [0; 3];
            peer.read(&mut reply).await.map_err(failure)
        })
    );
    let (result, reply) = observed(result, reply)?;
    let stats = result?;
    assert_eq!(reply, 0);
    assert_eq!(stats.packets_received, 1);
    assert_eq!(stats.packets_sent, 0);
    Ok(())
}

#[tokio::test]
async fn observed_peer_terminate_closes_without_a_termination_reply() -> Result<(), MediaError> {
    let Pair {
        link,
        mut peer,
        application: _application,
    } = pair().await?;
    bounded(async {
        peer.write_all(&AudioSocket::Terminate.encode()?)
            .await
            .map_err(failure)
    })
    .await?;
    let (result, reply) = tokio::join!(
        bounded(link.run()),
        bounded(async {
            let mut reply = [0; 3];
            peer.read(&mut reply).await.map_err(failure)
        })
    );
    let (result, reply) = observed(result, reply)?;
    let stats = result?;
    assert_eq!(reply, 0);
    assert_eq!(stats.packets_received, 1);
    assert_eq!(stats.packets_sent, 0);
    Ok(())
}

#[tokio::test]
async fn incomplete_peer_frames_remain_errors() -> Result<(), MediaError> {
    for bytes in [&[0x10][..], &[0x10, 0, 2, 1][..]] {
        let Pair {
            link,
            mut peer,
            application: _application,
        } = pair().await?;
        bounded(async {
            peer.write_all(bytes).await.map_err(failure)?;
            peer.shutdown().await.map_err(failure)
        })
        .await?;
        let (result, reply) = tokio::join!(bounded(link.run()), bounded(termination(&mut peer)));
        let (result, ()) = observed(result, reply)?;
        assert!(matches!(result, Err(MediaError::InvalidPacket)));
    }
    Ok(())
}

#[tokio::test]
async fn local_stop_still_sends_a_termination_frame() -> Result<(), MediaError> {
    let Pair {
        link,
        mut peer,
        application,
    } = pair().await?;
    application.stop.send_replace(true);
    let (result, reply) = tokio::join!(bounded(link.run()), bounded(termination(&mut peer)));
    let (result, ()) = observed(result, reply)?;
    let stats = result?;
    assert_eq!(stats.packets_received, 0);
    assert_eq!(stats.packets_sent, 0);
    Ok(())
}

#[tokio::test]
async fn peer_resets_remain_transport_errors() -> Result<(), MediaError> {
    let Pair {
        link,
        peer,
        application: _application,
    } = pair().await?;
    peer.set_zero_linger().map_err(failure)?;
    drop(peer);
    let result = bounded(link.run()).await;
    assert!(matches!(result, Err(MediaError::Transport(_))));
    Ok(())
}

#[tokio::test]
async fn local_stop_preserves_failed_termination_writes() -> Result<(), MediaError> {
    let Pair {
        mut link,
        peer,
        application,
    } = pair().await?;
    peer.set_zero_linger().map_err(failure)?;
    drop(peer);
    let reset = bounded(async {
        let mut input = [0; 1];
        link.socket.read(&mut input).await.map_err(failure)
    })
    .await;
    if !matches!(reset, Err(MediaError::Transport(_))) {
        return Err(failure("Reset was not observed before the local stop"));
    }
    application.stop.send_replace(true);
    let result = bounded(link.run()).await;
    assert!(matches!(result, Err(MediaError::Transport(_))));
    Ok(())
}
