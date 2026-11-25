use rusterisk_api::ChannelId;
use rusterisk_media::{AudioSocket, Frame, MediaError};
use rusterisk_server::{media::MediaHub, CallKey};
use rusterisk_test_support::Wait;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

async fn receive(peer: &mut TcpStream, expected: i16) -> Result<(), MediaError> {
    Wait::Ready.bounded(async {
        let mut header = [0; 3];
        let mut bytes = [0; 1923];
        for _ in 0..100 {
            peer.read_exact(&mut header).await.map_err(failure)?;
            let length = usize::from(u16::from_be_bytes([header[1], header[2]]));
            if length > 1920 { return Err(MediaError::Capacity); }
            bytes[..3].copy_from_slice(&header);
            peer.read_exact(&mut bytes[3..length + 3]).await.map_err(failure)?;
            if matches!(AudioSocket::parse(&bytes[..length + 3])?, AudioSocket::Audio(frame) if frame.samples() == [expected; 160]) { return Ok(()); }
        }
        Err(MediaError::InvalidFrame)
    }).await.map_err(failure)?
}

#[tokio::test]
async fn stream_links_share_the_mixer_and_preserve_received_audio_in_both_directions(
) -> Result<(), MediaError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let (mut left, (left_socket, _)) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().map_err(failure)?),
        listener.accept()
    )
    .map_err(failure)?;
    let (mut right, (right_socket, _)) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().map_err(failure)?),
        listener.accept()
    )
    .map_err(failure)?;
    let (media, hub, _notices) = MediaHub::new(4);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        let left_key = CallKey {
            id: ChannelId::try_from("left".to_owned()).map_err(failure)?,
            generation: 1,
        };
        let right_key = CallKey {
            id: ChannelId::try_from("right".to_owned()).map_err(failure)?,
            generation: 2,
        };
        media
            .attach_audiosocket(left_key.clone(), left_socket, 8000)
            .await?;
        media
            .attach_audiosocket(right_key.clone(), right_socket, 8000)
            .await?;
        media.bridge(
            1,
            [left_key.clone(), right_key.clone()].into_iter().collect(),
        )?;
        left.write_all(&AudioSocket::Audio(Frame::new(&[4000; 320], 8000)?).encode()?)
            .await
            .map_err(failure)?;
        right
            .write_all(&AudioSocket::Audio(Frame::new(&[-7000; 320], 8000)?).encode()?)
            .await
            .map_err(failure)?;
        tokio::try_join!(receive(&mut left, -7000), receive(&mut right, 4000))?;
        media.release(left_key)?;
        media.release(right_key)?;
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    result
}
