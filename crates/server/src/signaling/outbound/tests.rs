use crate::media::MediaHub;
use crate::signaling::calls;
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
use rusterisk_test_support::Wait;
use std::net::Ipv4Addr;
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::Instant;
mod authentication;
mod capacity;
mod starting_profile;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

// The composed loop applies a queued release before it takes the next completion.
// The test holds the release back to check that the completion still refuses the ended call on its own.
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
    let (controller, runner, _effects) = Controller::new(config.clone(), 64);
    let (media, _hub, notices) = MediaHub::new(64);
    let mut signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
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
    let result: Result<(), ControlError> = async {
        let credentials = || Credentials::new("app".into(), "test".into());
        let subscription = signaling.control_until(controller.subscribe(credentials()?, None, "app".into())).await??;
        let id = ChannelId::try_from("cancelled-resolution".to_owned())?;
        signaling.control_until(controller.execute(credentials()?, None, Action::Originate { id: id.clone(), endpoint: "PJSIP/phone".into(), app: "app".into(), args: Vec::new(), caller: "100".into(), timeout: 30 })).await??;
        let effect = signaling.next_effect_for_test().await?;
        let Effect::Start { key, .. } = &effect else { return Err(ControlError::Invalid("Expected call start")); };
        let key = key.clone();
        signaling.effect(effect, 0).await?;
        let mut bytes = [0; 2048];
        let (size, address) = signaling.control_until(Wait::Ready.bounded(dns.recv_from(&mut bytes))).await?.map_err(failure)?.map_err(failure)?;
        signaling.control_until(controller.execute(credentials()?, None, Action::Hangup(id))).await??;
        assert!(signaling.records.get(calls::NATIVE, &key).is_some());
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
        let completed = Wait::Ready.bounded(signaling.native_job_for_test()).await.map_err(failure)??.ok_or(ControlError::Capacity)?.map_err(failure)?;
        signaling.native_completed(completed, 1).await?;
        assert!(signaling.records.iter(calls::ACTIVE).next().is_none());
        assert!(signaling.records.iter(calls::NATIVE).next().is_none());
        assert!(signaling.resolution_cache.get(&StaticTarget::from_uri(Protocol::Tcp, &target).map_err(failure)?, Instant::now()).is_none());
        assert!(Wait::Quiet.bounded(peer.accept()).await.is_err());
        assert!(matches!(signaling.queued_effect_for_test(), Some(Effect::Release { key: released }) if released == key));
        drop(subscription);
        Ok(())
    }.await;
    controller.shutdown();
    signaling.finish_native().await?;
    signaling.network.close().await?;
    result
}
