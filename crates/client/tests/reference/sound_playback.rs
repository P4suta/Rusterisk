use rusterisk_client::{
    api::{ChannelId, ExternalRtpTarget, MediaUri, PlaybackState, SoundId, StaticRtpCodec},
    Client, Error, Playback, Session,
};
use rusterisk_server::rtp::SocketPair;
use std::net::{Ipv4Addr, SocketAddr};

#[derive(Clone, Copy)]
enum Target {
    Channel,
    Bridge,
}

fn accepted(playback: Playback, id: &rusterisk_client::PlaybackId, target: &str, language: &str) {
    assert_eq!(playback.id, *id);
    assert_eq!(playback.media_uri, "sound:digits/rusterisk-one");
    assert_eq!(playback.target_uri, target);
    assert_eq!(playback.language, language);
    assert_eq!(playback.next_media_uri, None);
    assert!(matches!(
        playback.state,
        PlaybackState::Queued | PlaybackState::Playing
    ));
}

pub async fn compare(
    client: &Client,
    session: &mut Session,
    application: &str,
    callback: Ipv4Addr,
) -> Result<(), Error> {
    let media = MediaUri::Sound(
        SoundId::try_from("digits/rusterisk-one".to_owned())
            .map_err(|_| Error::Invalid("sound fixture name"))?,
    );
    for (language, sample) in [("en", 1234), ("ja", 2345)] {
        for target in [Target::Channel, Target::Bridge] {
            let label = match target {
                Target::Channel => "channel",
                Target::Bridge => "bridge",
            };
            let id = ChannelId::try_from(format!("sound-{label}-{language}"))?;
            let bridge = rusterisk_client::BridgeId::try_from(format!("sound-{label}-{language}"))?;
            let playback =
                rusterisk_client::PlaybackId::try_from(format!("play-{label}-{language}"))?;
            let language = ChannelId::try_from(language.to_owned())?;
            let peer = SocketPair::bind(Ipv4Addr::UNSPECIFIED.into(), 0, 0)?;
            let address = SocketAddr::new(callback.into(), peer.local_addr()?.port());
            let destination = ExternalRtpTarget::new(address, StaticRtpCodec::Pcmu)
                .map_err(|_| Error::Invalid("sound playback peer"))?;
            client
                .external_rtp(&id, application, destination, "sound")
                .await?;
            super::external_audio::started(session, &id).await?;
            match target {
                Target::Channel => accepted(
                    client
                        .play_channel(&id, &playback, &media, Some(&language))
                        .await?,
                    &playback,
                    &format!("channel:{id}"),
                    language.as_str(),
                ),
                Target::Bridge => {
                    client.create_bridge(&bridge, "Sound playback").await?;
                    client
                        .add_channels(&bridge, std::slice::from_ref(&id))
                        .await?;
                    accepted(
                        client
                            .play_bridge(&bridge, &playback, &media, Some(&language))
                            .await?,
                        &playback,
                        &format!("bridge:{id}"),
                        language.as_str(),
                    );
                }
            }
            super::external_audio::received(&peer, StaticRtpCodec::Pcmu, sample).await?;
            client.hangup(&id).await?;
            if matches!(target, Target::Bridge) {
                client.destroy_bridge(&bridge).await?;
            }
        }
    }
    Ok(())
}
