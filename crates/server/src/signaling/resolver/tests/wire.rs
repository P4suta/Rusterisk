use super::{failure, DestinationResolver};
use crate::ControlError;
use hickory_resolver::{
    config::{LookupIpStrategy, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts},
    proto::op::{Message, OpCode},
};
use rusterisk_config::Protocol;
use rusterisk_sip::StaticTarget;
use rusterisk_test_support::Wait;
use tokio::net::UdpSocket;

fn resolver(socket: &UdpSocket) -> Result<DestinationResolver, ControlError> {
    let address = socket.local_addr().map_err(failure)?;
    let mut server = NameServerConfig::udp(address.ip());
    server
        .connections
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .port = address.port();
    let mut options = ResolverOpts::default();
    options.ip_strategy = LookupIpStrategy::Ipv4Only;
    options.use_hosts_file = ResolveHosts::Never;
    DestinationResolver::with_config(
        ResolverConfig::from_name_servers(std::iter::once(server).collect()),
        options,
    )
}

enum QueryEnd {
    Deadline,
    Cancelled,
}

#[tokio::test]
async fn deadlines_and_cancellation_release_each_query_socket() -> Result<(), ControlError> {
    for end in [QueryEnd::Deadline, QueryEnd::Cancelled] {
        let socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let resolver = resolver(&socket)?;
        let query_resolver = resolver.clone();
        let target =
            StaticTarget::from_uri(Protocol::Tcp, "sip:phone@peer.test:5070").map_err(failure)?;
        let job = tokio::spawn(async move { query_resolver.resolve(&target).await });
        let mut bytes = [0; 2048];
        let (size, source) = Wait::Ready
            .bounded(socket.recv_from(&mut bytes))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
        assert!(size <= 1232);
        assert_eq!(
            request.edns.ok_or(ControlError::Capacity)?.max_payload(),
            1232
        );
        match end {
            QueryEnd::Deadline => assert!(Wait::Ready
                .bounded(job)
                .await
                .map_err(failure)?
                .map_err(failure)?
                .is_err()),
            QueryEnd::Cancelled => {
                job.abort();
                assert!(job.await.is_err_and(|error| error.is_cancelled()));
            }
        }
        let released = UdpSocket::bind(source).await.map_err(failure)?;
        assert_eq!(released.local_addr().map_err(failure)?, source);
        drop((released, resolver));
    }
    Ok(())
}

#[tokio::test]
async fn impossible_answer_counts_fail_within_the_query_deadline() -> Result<(), ControlError> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let resolver = resolver(&socket)?;
    let target =
        StaticTarget::from_uri(Protocol::Udp, "sip:phone@peer.test:5070").map_err(failure)?;
    let job = tokio::spawn(async move { resolver.resolve(&target).await });
    let mut bytes = [0; 2048];
    let (size, source) = Wait::Ready
        .bounded(socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
    let mut response = Message::response(request.metadata.id, OpCode::Query);
    response.metadata.authoritative = true;
    response.metadata.recursion_available = true;
    response.queries = request.queries;
    response.edns = request.edns;
    let mut response = response.to_vec().map_err(failure)?;
    response
        .get_mut(6..8)
        .ok_or(ControlError::Capacity)?
        .copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(response.len() < 1232);
    assert_eq!(
        socket.send_to(&response, source).await.map_err(failure)?,
        response.len()
    );
    assert!(Wait::Ready
        .bounded(job)
        .await
        .map_err(failure)?
        .map_err(failure)?
        .is_err());
    let released = UdpSocket::bind(source).await.map_err(failure)?;
    assert_eq!(released.local_addr().map_err(failure)?, source);
    Ok(())
}
