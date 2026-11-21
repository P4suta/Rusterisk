use rusterisk_client::{
    api::{ChannelId, Dialplan, EventKind, EventSource, ExternalRtpTarget, StaticRtpCodec},
    Client, Error, Session,
};
use std::{net::Ipv4Addr, time::Duration};

async fn entry(
    session: &mut Session,
    id: &ChannelId,
    expected_data: Option<&str>,
) -> Result<(Dialplan, Vec<String>), Error> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut observed = expected_data.is_none();
        loop {
            let kind = session.next_event().await?.kind;
            observed |= matches!(&kind, EventKind::ChannelDialplan { channel, dialplan_app, dialplan_app_data }
                if channel.id == *id && dialplan_app == "Stasis" && expected_data == Some(dialplan_app_data.as_str()));
            if let EventKind::StasisStart { channel, args } = kind {
                if channel.id == *id {
                    if !observed {
                        return Err(Error::Invalid("StasisStart preceded the subscribed ChannelDialplan"));
                    }
                    return Ok((channel.dialplan, args));
                }
            }
        }
    })
    .await
    .map_err(|_| Error::Invalid("dialplan Stasis entry timed out"))?
}

pub(super) async fn compare(
    client: &Client,
    session: &mut Session,
    app: &str,
) -> Result<(), Error> {
    let peer = rusterisk_server::rtp::SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0)?;
    for (index, (dialed, selected, context, priority)) in [
        ("7", "early", "route-order", 1),
        ("234", "narrow", "route-order", 1),
        ("123", "wide", "route-order", 1),
        ("300", "exact", "route-order", 1),
        ("+12", "jump", "route-result", 2),
    ]
    .into_iter()
    .enumerate()
    {
        let id = ChannelId::try_from(format!("dialplan-contract-{index}"))?;
        let target = ExternalRtpTarget::new(peer.local_addr()?, StaticRtpCodec::Pcmu)
            .map_err(|_| Error::Invalid("external dialplan peer"))?;
        client.external_rtp(&id, app, target, "").await?;
        let result = async {
            entry(session, &id, None).await?;
            client
                .subscribe_resources(app, &[EventSource::Channel(id.clone())])
                .await?;
            client
                .continue_in_dialplan(&id, Some("route-order"), Some(dialed), Some(1))
                .await?;
            let data = format!("{app},{selected},{dialed}");
            let (location, args) = entry(session, &id, Some(&data)).await?;
            assert_eq!(
                (
                    location.context.as_str(),
                    location.exten.as_str(),
                    location.priority
                ),
                (context, dialed, priority),
                "{dialed}"
            );
            assert_eq!(args, [selected, dialed], "{dialed}");
            Ok::<_, Error>(())
        }
        .await;
        let cleanup = client.hangup(&id).await;
        result?;
        cleanup?;
    }
    Ok(())
}
