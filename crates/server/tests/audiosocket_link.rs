use rusterisk_core::{DtmfDigit, PeerMediaDirection};
use rusterisk_media::{AudioSocket, Frame, MediaError};
use rusterisk_server::{audiosocket::AudioSocketLink, media_link::Channels};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, watch},
};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

#[tokio::test]
async fn partial_stream_reads_survive_outgoing_audio_and_close_with_the_link(
) -> Result<(), MediaError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let (mut peer, (socket, _)) = tokio::try_join!(
        tokio::net::TcpStream::connect(listener.local_addr().map_err(failure)?),
        listener.accept()
    )
    .map_err(failure)?;
    let (incoming, mut received) = mpsc::channel(3);
    let (outgoing, output) = mpsc::channel(3);
    let (digits, mut received_digits) = mpsc::channel(16);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completions) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (_direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
    let link = AudioSocketLink::new(
        socket,
        Channels {
            incoming,
            outgoing: output,
            digits,
            commands,
            completed,
            stop: stopped,
            direction: directed,
        },
        8000,
    )?;
    let task = tokio::spawn(link.run());
    let result = async {
        let bytes = AudioSocket::Audio(Frame::new(&[4000; 160], 8000)?).encode()?;
        peer.write_all(&bytes[..2]).await.map_err(failure)?;
        outgoing
            .send(Frame::new(&[-7000; 160], 8000)?)
            .await
            .map_err(failure)?;
        let mut response = [0; 323];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut response))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert_eq!(
            AudioSocket::parse(&response)?,
            AudioSocket::Audio(Frame::new(&[-7000; 160], 8000)?)
        );
        for chunk in bytes[2..].chunks(7) {
            peer.write_all(chunk).await.map_err(failure)?;
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .map_err(failure)?,
            Some(Frame::new(&[4000; 160], 8000)?)
        );
        peer.write_all(&AudioSocket::Dtmf(b'#').encode()?)
            .await
            .map_err(failure)?;
        let tone = tokio::time::timeout(Duration::from_secs(1), received_digits.recv())
            .await
            .map_err(failure)?
            .ok_or(MediaError::Capacity)?;
        assert_eq!(tone.digit, DtmfDigit::Hash);
        assert_eq!(tone.duration_ms, 0);
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    result
}
