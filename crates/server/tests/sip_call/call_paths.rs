use super::{
    failure,
    loopback_extensions::{call, native},
    process::combine,
    registration::register,
    transfer_peer::Peer,
    transfer_reference_configuration::Configuration,
};
use rusterisk_config::{Auth, Config};
use rusterisk_test_support::Wait;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use rusterisk_server::ControlError;

#[derive(Clone, Copy)]
pub(super) enum CallPath {
    RegisteredExtensions,
    IncomingTrunk,
    OutgoingTrunk,
}

#[tokio::test]
async fn the_test_trunk_can_call_a_registered_phone() -> Result<(), ControlError> {
    native(CallPath::IncomingTrunk).await
}

#[tokio::test]
async fn a_registered_phone_can_dial_the_test_trunk() -> Result<(), ControlError> {
    native(CallPath::OutgoingTrunk).await
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn reference_registered_extensions() -> Result<(), ControlError> {
    compare(CallPath::RegisteredExtensions).await
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn reference_incoming_trunk() -> Result<(), ControlError> {
    compare(CallPath::IncomingTrunk).await
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn reference_outgoing_trunk() -> Result<(), ControlError> {
    compare(CallPath::OutgoingTrunk).await
}

pub(super) struct Phones {
    alice: Peer,
    bob: Peer,
    gateway: Peer,
}

impl Phones {
    pub(super) async fn bind(bind: IpAddr, advertised: IpAddr) -> Result<Self, ControlError> {
        Ok(Self {
            alice: Peer::bind(bind, advertised).await?,
            bob: Peer::bind(bind, advertised).await?,
            gateway: Peer::bind(bind, advertised).await?,
        })
    }

    pub(super) fn settings(&self) -> Result<(String, String), ControlError> {
        let gateway = self.gateway.contact()?;
        let sip = format!("{}\n[gateway-auth]\ntype=auth\nauth_type=userpass\nusername=gateway\npassword=development-gateway\n[gateway]\ntype=aor\ncontact=sip:gateway@{gateway}\n[gateway]\ntype=endpoint\ntransport=udp\nauth=gateway-auth\naors=gateway\ncontext=from-line\ndisallow=all\nallow=ulaw\n", include_str!("../../../../config/pjsip.conf"));
        let extensions = format!("{}exten => 901,1,Dial(PJSIP/5551200@gateway)\n[from-line]\nexten => 5550101,1,Dial(PJSIP/alice)\n", include_str!("../../../../config/extensions.conf"));
        Ok((sip, extensions))
    }

    pub(super) async fn exercise(
        &self,
        config: &Config,
        target: SocketAddr,
        path: CallPath,
    ) -> Result<(), ControlError> {
        let account = |name: &str| -> Result<&Auth, ControlError> {
            config
                .auth
                .iter()
                .find(|entry| entry.username == name)
                .ok_or(ControlError::Invalid("Missing SIP fixture account"))
        };
        let alice = account("alice")?;
        let bob = account("bob")?;
        for (serial, phone, auth) in [(1, &self.alice, alice), (2, &self.bob, bob)] {
            let count = register(
                &phone.signal,
                target,
                phone.contact()?,
                &auth.username,
                auth.password.expose(),
                serial,
                300,
            )
            .await?;
            if count != 1 {
                return Err(ControlError::Invalid(
                    "Expected one registered phone contact",
                ));
            }
        }
        let call_result = async {
            match path {
                CallPath::RegisteredExtensions => {
                    call(target, &self.alice, alice, "102", &self.bob, "bob").await?;
                    call(target, &self.bob, bob, "101", &self.alice, "alice").await
                }
                CallPath::IncomingTrunk => {
                    call(
                        target,
                        &self.gateway,
                        account("gateway")?,
                        "5550101",
                        &self.alice,
                        "alice",
                    )
                    .await
                }
                CallPath::OutgoingTrunk => {
                    call(target, &self.alice, alice, "901", &self.gateway, "5551200").await
                }
            }
        }
        .await;
        let mut result = call_result;
        for (serial, phone, auth) in [(3, &self.alice, alice), (4, &self.bob, bob)] {
            let cleared = register(
                &phone.signal,
                target,
                phone.contact()?,
                &auth.username,
                auth.password.expose(),
                serial,
                0,
            )
            .await
            .and_then(|count| {
                if count == 0 {
                    Ok(())
                } else {
                    Err(ControlError::Invalid("Registered contact survived removal"))
                }
            });
            result = combine(result, cleared);
        }
        result
    }
}

async fn compare(path: CallPath) -> Result<(), ControlError> {
    native(path).await?;
    let reference = Configuration::load().await?;
    let previous = BTreeSet::new();
    if !reference.channels().await?.is_empty() {
        return Err(ControlError::Invalid("Reference has active channels"));
    }
    let address = |name| -> Result<Ipv4Addr, ControlError> {
        std::env::var(name)
            .map_err(failure)?
            .parse()
            .map_err(failure)
    };
    let callback = address("RUSTERISK_REFERENCE_CALLBACK_IP")?;
    let server = address("RUSTERISK_REFERENCE_MEDIA_IP")?;
    if callback.is_unspecified()
        || callback.is_multicast()
        || callback.is_broadcast()
        || !server.is_private()
    {
        return Err(ControlError::Invalid("Invalid reference peer address"));
    }
    let phones = Phones::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let (sip, extensions) = phones.settings()?;
    let config = Config::parse(
        include_str!("../../../../config/http.conf"),
        include_str!("../../../../config/ari.conf"),
        &sip,
        include_str!("../../../../config/rtp.conf"),
        &extensions,
    )
    .map_err(failure)?;
    let sip = format!(
        "[global]\ntype=global\nendpoint_identifier_order=auth_username,username,ip\n{}",
        sip.replace("127.0.0.1:5060", "0.0.0.0:5060")
            .replace(
                "auth_type=userpass",
                "auth_type=userpass\nsupported_algorithms_uas=MD5"
            )
            .replace(
                "type=endpoint",
                "type=endpoint\nidentify_by=auth_username\ndirect_media=no"
            )
    );
    let result = async {
        reference.install(&sip, extensions.as_bytes()).await?;
        let task = tokio::spawn(async move {
            Wait::Scenario
                .bounded(phones.exercise(&config, SocketAddr::new(server.into(), 5060), path))
                .await
                .map_err(failure)?
        });
        task.await.map_err(failure)??;
        reference.wait_for_channels(&previous).await
    }
    .await;
    let result = combine(result, reference.end_channels(&previous).await);
    combine(result, reference.restore().await)
}
