use super::{answer, credentials, failure, id, late_answer, matching_event, transfer_peer::Peer};
use rsipstack::sip::{HeadersExt, Method};
use rusterisk_api::{ChannelState, EventKind};
use rusterisk_config::{Codec, Config};
use rusterisk_server::{runtime::Server, Action, ControlError};
use rusterisk_sip::Session;
use serde_json::Value;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Echo,
    Busy,
    Unanswered,
    Silent,
    Wrong,
    Interrupt,
}

async fn audio(peer: &Peer, scenario: Scenario) -> Result<(), ControlError> {
    if matches!(scenario, Scenario::Busy | Scenario::Unanswered) {
        let (invite, target) = late_answer::receive(&peer.signal, Method::Invite).await?;
        let to = format!("{};tag=probe", invite.to_header().map_err(failure)?.value());
        let status = if scenario == Scenario::Busy {
            "486 Busy Here"
        } else {
            "180 Ringing"
        };
        peer.signal
            .send_to(
                late_answer::reply(&invite, status, &to, "", "")?.as_bytes(),
                target,
            )
            .await
            .map_err(failure)?;
        return Ok(());
    }
    let target = answer(&peer.signal, &peer.audio).await?;
    let mut data = [0; 2048];
    loop {
        let (len, source) = peer
            .audio
            .data()
            .recv_from(&mut data)
            .await
            .map_err(failure)?;
        assert_eq!(source, target);
        if len < 12 {
            return Err(ControlError::Invalid("Incomplete test packet"));
        }
        let packet = &mut data[..len];
        match scenario {
            Scenario::Echo => {}
            Scenario::Wrong => packet[12..].fill(0x90),
            Scenario::Silent => packet[12..].fill(0xff),
            Scenario::Busy | Scenario::Unanswered | Scenario::Interrupt => {
                return Err(ControlError::Invalid("Unexpected media scenario"))
            }
        }
        peer.audio
            .data()
            .send_to(packet, target)
            .await
            .map_err(failure)?;
    }
}

async fn interrupt(client: &reqwest::Client, url: &str, peer: &Peer) -> Result<(), ControlError> {
    let response = client
        .post(url)
        .basic_auth("app", Some("test-password"))
        .send();
    let (reply, received) =
        tokio::join!(response, late_answer::receive(&peer.signal, Method::Invite));
    let reply = reply.map_err(failure)?;
    assert_eq!(reply.status(), 200);
    let (invite, target) = received?;
    let to = format!(
        "{};tag=late-probe",
        invite.to_header().map_err(failure)?.value()
    );
    peer.signal
        .send_to(
            late_answer::reply(&invite, "180 Ringing", &to, "", "")?.as_bytes(),
            target,
        )
        .await
        .map_err(failure)?;
    drop(reply);
    let (cancel, _) = late_answer::receive(&peer.signal, Method::Cancel).await?;
    late_answer::cancellation(&invite, &cancel)?;
    peer.signal
        .send_to(
            late_answer::reply(&cancel, "200 OK", &to, "", "")?.as_bytes(),
            target,
        )
        .await
        .map_err(failure)?;
    let session = Session::parse(
        std::str::from_utf8(&invite.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?;
    let sdp = session.answer(peer.media()?, 1).map_err(failure)?;
    let headers = format!(
        "Contact: <sip:echo@{}>\r\nContent-Type: application/sdp\r\n",
        peer.contact()?
    );
    peer.signal
        .send_to(
            late_answer::reply(&invite, "200 OK", &to, &headers, &sdp)?.as_bytes(),
            target,
        )
        .await
        .map_err(failure)?;
    let (ack, _) = late_answer::receive(&peer.signal, Method::Ack).await?;
    let (bye, _) = late_answer::receive(&peer.signal, Method::Bye).await?;
    late_answer::teardown(&ack, &bye)?;
    peer.signal
        .send_to(
            late_answer::reply(&bye, "200 OK", &to, "", "")?.as_bytes(),
            target,
        )
        .await
        .map_err(failure)?;
    Ok(())
}

async fn probe(scenario: Scenario) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let peer = Peer::bind(ip, ip).await?;
    let keep = Peer::bind(ip, ip).await?;
    let mut sip="[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n".to_owned();
    for (name, address) in [("echo", peer.contact()?), ("keep", keep.contact()?)] {
        sip.push_str(&format!("[{name}]\ntype=aor\ncontact=sip:phone@{address}\n[{name}]\ntype=endpoint\ntransport=udp\nauth=auth\naors={name}\nallow=ulaw\n"));
    }
    let mut config=Config::parse("[general]\nbindaddr=127.0.0.1\n","[app]\ntype=user\npassword=test-password\n[reader]\ntype=user\nread_only=yes\npassword=read\n",&sip,"","").map_err(failure)?;
    config.http.bind.set_port(0);
    config.rtp.start = 0;
    config.rtp.end = 0;
    let root = tempfile::tempdir().map_err(failure)?;
    let server = Server::bind(config, 8, root.path().into()).await?;
    let address: SocketAddr = server.local_addr()?;
    let controller = server.controller();
    let run = tokio::spawn(server.run());
    let result=async {
        let mut app=controller.subscribe(credentials()?,None,"keep-app".into()).await?;
        let keep_id=id("keep-leg")?;
        controller.execute(credentials()?,None,Action::Originate{id:keep_id.clone(),endpoint:"PJSIP/keep".into(),app:"keep-app".into(),args:Vec::new(),caller:String::new(),timeout:20}).await?;
        let _remote=answer(&keep.signal,&keep.audio).await?;
        matching_event(&mut app.events,|e|matches!(e,EventKind::StasisStart{channel,..} if channel.id==keep_id && channel.state==ChannelState::Up)).await?;
        let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(8)).build().map_err(failure)?;
        let limit=if scenario==Scenario::Interrupt{6}else{2};
        let url=format!("http://{address}/rusterisk/test-calls/PJSIP/echo?deadline_seconds={limit}");
        assert_eq!(client.post(&url).basic_auth("reader",Some("read")).send().await.map_err(failure)?.status(),403);
        if scenario==Scenario::Interrupt {interrupt(&client,&url,&peer).await?;} else {
            let request=async {
                let reply=client.post(&url).basic_auth("app",Some("test-password")).send().await.map_err(failure)?;
                assert_eq!(reply.status(),200);
                Ok::<Value,ControlError>(serde_json::from_slice(&reply.bytes().await.map_err(failure)?)?)
            };
            tokio::pin!(request);
            let result=tokio::select!{out=&mut request=>out?,done=audio(&peer,scenario)=>{done?;request.await?}};
            let (outcome,answered)=match scenario {
                Scenario::Echo=>("verified",true),Scenario::Busy=>("busy",false),Scenario::Unanswered=>("no_answer",false),Scenario::Wrong=>("signal_mismatch",true),Scenario::Silent=>("silent",true),Scenario::Interrupt=>return Err(ControlError::Invalid("Unexpected completion")),
            };
            assert_eq!(result["outcome"],outcome);
            assert_eq!(result["accepted"],true);
            assert_eq!(result["answered"],answered);
            assert_eq!(result["signal_verified"],scenario==Scenario::Echo);
        }
        tokio::time::timeout(Duration::from_secs(2),async {
            loop {
                let calls=controller.execute(credentials()?,None,Action::ListChannels).await?.body.ok_or(ControlError::Capacity)?;
                if let Some([one])=calls.as_array().map(Vec::as_slice){assert_eq!(one["id"],"keep-leg");assert_eq!(one["state"],"Up");break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let bridges=controller.execute(credentials()?,None,Action::ListBridges).await?.body.ok_or(ControlError::Capacity)?;
            assert_eq!(bridges.as_array().map(Vec::len),Some(0));
            Ok::<(),ControlError>(())
        }).await.map_err(failure)??;
        Ok(())
    }.await;
    controller.shutdown();
    run.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn test_calls_verify_echo_and_retire_only_the_nominated_call() -> Result<(), ControlError> {
    for scenario in [
        Scenario::Echo,
        Scenario::Busy,
        Scenario::Unanswered,
        Scenario::Silent,
        Scenario::Wrong,
        Scenario::Interrupt,
    ] {
        probe(scenario).await?;
    }
    Ok(())
}
