use crate::media::MediaHub;
use crate::signaling::{resolver::DestinationResolver, Signaling};
use crate::{Action, ControlError, Controller, Credentials, Effect};
use hickory_resolver::{
    config::{LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts},
    proto::{
        op::{Message, OpCode},
        rr::{rdata::A, RData, Record},
    },
};
use rusterisk_api::ChannelId;
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::StaticTarget;
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use tokio::net::{TcpListener, UdpSocket};
mod authentication;
mod capacity;
mod starting_profile;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn cancelled_resolution_cannot_connect_before_the_release_effect_is_consumed(
) -> Result<(), ControlError> {
    let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let peer = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let target = format!(
        "sip:phone@peer.test:{}",
        peer.local_addr().map_err(failure)?.port()
    );
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", &format!("[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=password\n[static]\ntype=aor\ncontact={target}\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=static\nallow=ulaw\n"), "", "").map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 64);
    let (media, _hub, notices) = MediaHub::new(64);
    let mut signaling =
        Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let mut server = NameServerConfig::udp(Ipv4Addr::LOCALHOST.into());
    server
        .connections
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .port = dns.local_addr().map_err(failure)?.port();
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    options.use_hosts_file = ResolveHosts::Never;
    signaling.resolver = DestinationResolver::with_config(
        ResolverConfig::from_name_servers(std::iter::once(server).collect()),
        options,
    );
    let runner = tokio::spawn(runner.run());
    let result: Result<(), ControlError> = async {
        let credentials = || Credentials::new("app".into(), "test".into());
        let subscription = controller.subscribe(credentials()?, None, "app".into()).await?;
        let id = ChannelId::try_from("cancelled-resolution".to_owned())?;
        controller.execute(credentials()?, None, Action::Originate { id: id.clone(), endpoint: "PJSIP/phone".into(), app: "app".into(), args: Vec::new(), caller: "100".into(), timeout: 30 }).await?;
        let effect = signaling.effects.recv().await.ok_or(ControlError::Capacity)?;
        let Effect::Start { key, .. } = &effect else { return Err(ControlError::Invalid("Expected call start")); };
        let key = key.clone();
        signaling.effect(effect, 0).await?;
        let mut bytes = [0; 2048];
        let (size, address) = tokio::time::timeout(Duration::from_secs(2), dns.recv_from(&mut bytes)).await.map_err(failure)?.map_err(failure)?;
        controller.execute(credentials()?, None, Action::Hangup(id)).await?;
        assert!(signaling.native_pending.contains_key(&key));
        let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
        let query = request.queries.first().ok_or(ControlError::Capacity)?.clone();
        let mut reply = Message::response(request.metadata.id, OpCode::Query);
        reply.metadata.authoritative = true;
        reply.metadata.recursion_available = true;
        reply.metadata.recursion_desired = request.metadata.recursion_desired;
        reply.queries.push(query.clone());
        reply.answers.push(Record::from_rdata(query.name().clone(), 60, RData::A(A(Ipv4Addr::LOCALHOST))));
        reply.edns = request.edns;
        let reply = reply.to_vec().map_err(failure)?;
        assert_eq!(dns.send_to(&reply, address).await.map_err(failure)?, reply.len());
        let completed = tokio::time::timeout(Duration::from_secs(2), signaling.native_jobs.join_next()).await.map_err(failure)?.ok_or(ControlError::Capacity)?.map_err(failure)?;
        signaling.native_completed(completed, 1).await?;
        assert!(signaling.calls.is_empty());
        assert!(signaling.native_pending.is_empty());
        assert!(signaling.resolution_cache.get(&StaticTarget::from_uri(Protocol::Tcp, &target).map_err(failure)?, Instant::now()).is_none());
        assert!(tokio::time::timeout(Duration::from_millis(100), peer.accept()).await.is_err());
        assert!(matches!(signaling.effects.try_recv(), Ok(Effect::Release { key: released }) if released == key));
        drop(subscription);
        Ok(())
    }.await;
    controller.shutdown();
    signaling.finish_native().await?;
    signaling.network.close().await?;
    runner.await.map_err(failure)??;
    result
}
