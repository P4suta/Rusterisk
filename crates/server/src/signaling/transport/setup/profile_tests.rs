use super::{Network, Phase, Prepared, SetupEvent};
use crate::{encryption::TlsClientContext, signaling::cleanup, ControlError};
use hickory_resolver::proto::rr::Name;
use rusterisk_config::{Config, Protocol, RegistrarUri, RegistrationAddress, RegistrationUriError};
use std::num::NonZeroU64;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct Fixture {
    network: Network,
    changed: TlsClientContext,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let identity =
            rcgen::generate_simple_self_signed(["provider.test".into()]).map_err(failure)?;
        let changed = rcgen::generate_simple_self_signed(["other.test".into()]).map_err(failure)?;
        let certificate = directory.path().join("certificate.pem");
        let private_key = directory.path().join("private-key.pem");
        let other_roots = directory.path().join("other-roots.pem");
        std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
        std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
        std::fs::write(&other_roots, changed.cert.pem()).map_err(failure)?;
        let mut config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test\n",
            &format!("[native]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:0\ncert_file={}\npriv_key_file={}\nca_list_file={}\n", certificate.display(), private_key.display(), certificate.display()),
            "",
            "",
        ).map_err(failure)?;
        Ok(Self {
            network: Network::bind(&mut config).await?,
            changed: TlsClientContext::load_sip(Some(other_roots.to_string_lossy().as_ref()))?,
            _directory: directory,
        })
    }

    fn replace_profile(&mut self) -> Result<(), ControlError> {
        let Some(super::super::ListenerSocket::Encrypted { client, .. }) =
            self.network.listeners.first_mut()
        else {
            return Err(ControlError::Invalid("Expected encrypted listener"));
        };
        *client = self.changed.clone();
        Ok(())
    }

    async fn finish(mut self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        cleanup::combine(primary, self.network.close().await)
    }
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Matching,
    EquivalentDomain,
    MultipleRootDots,
    CapturedBeforeReplacement,
    ChangedRoots,
    ChangedGeneration,
    DifferentDomain,
    Unverified,
    ClosedReceiver,
    Stopped,
    Closing,
    Cancelled,
    Stale,
}

async fn admission(case: Case) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary: Result<(), ControlError> = async {
        let first = fixture.network.reserve_setup(0, NonZeroU64::MIN)?;
        let profile = fixture
            .network
            .preparations
            .get(&first)
            .and_then(|preparation| preparation.tls_profile.clone())
            .ok_or(ControlError::Invalid(
                "TLS preparation lost its starting profile",
            ))?;
        fixture.network.finish_resolution(first);
        let peer = "127.0.0.1:5071".parse().map_err(failure)?;
        let super::Reserved {
            key: existing,
            packets,
            stop,
        } = fixture.network.reserve(0, peer)?;
        let mut owners = Some((packets, stop));
        let port = fixture
            .network
            .ports
            .get_mut(&(0, peer))
            .ok_or(ControlError::Capacity)?;
        port.identity = Some("provider.test".into());
        port.tls_profile = Some(profile);
        match case {
            Case::EquivalentDomain => port.identity = Some("PrOvIdEr.TeSt.".into()),
            Case::MultipleRootDots => port.identity = Some("provider.test..".into()),
            Case::DifferentDomain => port.identity = Some("edge.test".into()),
            Case::Unverified => {
                port.identity = None;
                port.tls_profile = None;
            }
            Case::Matching
            | Case::CapturedBeforeReplacement
            | Case::ChangedRoots
            | Case::ChangedGeneration
            | Case::ClosedReceiver
            | Case::Closing
            | Case::Cancelled
            | Case::Stale
            | Case::Stopped => {}
        }
        if matches!(case, Case::ChangedRoots) {
            fixture.replace_profile()?;
        }
        let generation = if matches!(case, Case::ChangedGeneration) {
            NonZeroU64::new(2).ok_or(ControlError::Capacity)?
        } else {
            NonZeroU64::MIN
        };
        let setup = fixture.network.reserve_setup(0, generation)?;
        if matches!(case, Case::CapturedBeforeReplacement) {
            fixture.replace_profile()?;
        }
        let preparation = fixture
            .network
            .preparations
            .get_mut(&setup)
            .ok_or(ControlError::Capacity)?;
        preparation.phase = Phase::Connecting;
        preparation.identity = Some("provider.test".into());
        match case {
            Case::Matching
            | Case::EquivalentDomain
            | Case::MultipleRootDots
            | Case::CapturedBeforeReplacement
            | Case::ChangedRoots
            | Case::ChangedGeneration
            | Case::DifferentDomain
            | Case::Unverified => {}
            Case::ClosedReceiver => drop(owners.take()),
            Case::Closing => {
                fixture
                    .network
                    .ports
                    .get_mut(&(0, peer))
                    .ok_or(ControlError::Capacity)?
                    .state =
                    super::ConnectionState::Closing(ControlError::Conflict("Stream closing"));
            }
            Case::Cancelled => fixture.network.cancel_setup(setup),
            Case::Stopped => {
                fixture
                    .network
                    .ports
                    .get(&(0, peer))
                    .ok_or(ControlError::Capacity)?
                    .stop
                    .send_replace(true);
            }
            Case::Stale => {
                if !matches!(
                    fixture.network.prepared_event(SetupEvent::Finished {
                        key: setup,
                        result: Ok(()),
                    }),
                    Some(super::super::Input::SetupFinished { .. })
                ) {
                    return Err(ControlError::Invalid("Stale setup did not retire"));
                }
                let replacement = fixture.network.reserve_setup(0, generation)?;
                if replacement == setup {
                    return Err(ControlError::Invalid("Setup generation was reused"));
                }
            }
        }
        let (reply, _reserved) = tokio::sync::oneshot::channel();
        let result = fixture.network.admit_prepared(Prepared {
            key: setup,
            peer,
            reply,
        });
        let matches = matches!(
            case,
            Case::Matching | Case::EquivalentDomain | Case::CapturedBeforeReplacement
        );
        match result {
            Ok(key) if matches && key == existing => {}
            Err(ControlError::Conflict(_)) if !matches => {}
            Ok(key) => {
                return Err(ControlError::Adapter(format!(
                    "TLS profile {case:?} reused unexpected connection {key:?}"
                )))
            }
            Err(error) => {
                return Err(ControlError::Adapter(format!(
                    "TLS profile {case:?} rejected its valid snapshot: {error}"
                )))
            }
        }
        let port = fixture
            .network
            .ports
            .get(&(0, peer))
            .ok_or(ControlError::Capacity)?;
        if port.key != existing || *port.stop.borrow() != matches!(case, Case::Stopped) {
            return Err(ControlError::Invalid(
                "Rejected preparation changed the admitted flow",
            ));
        }
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn prepared_tls_profiles_reject_replaced_settings_and_preserve_admitted_flow_ownership(
) -> Result<(), ControlError> {
    for case in [
        Case::Matching,
        Case::EquivalentDomain,
        Case::MultipleRootDots,
        Case::CapturedBeforeReplacement,
        Case::ChangedRoots,
        Case::ChangedGeneration,
        Case::DifferentDomain,
        Case::Unverified,
        Case::ClosedReceiver,
        Case::Stopped,
        Case::Closing,
        Case::Cancelled,
        Case::Stale,
    ] {
        admission(case).await?;
    }
    Ok(())
}

#[test]
fn malformed_dns_root_labels_cannot_enter_registration_settings() -> Result<(), ControlError> {
    for host in ["provider.test..", "provider.test..."] {
        assert!(Name::from_utf8(host).is_err());
        assert!(matches!(
            RegistrarUri::parse(Protocol::Tls, &format!("sips:{host}")),
            Err(RegistrationUriError::Invalid)
        ));
        assert!(matches!(
            RegistrationAddress::parse(&format!("sips:account@{host}")),
            Err(RegistrationUriError::Invalid)
        ));
    }
    RegistrarUri::parse(Protocol::Tls, "sips:provider.test.").map_err(failure)?;
    RegistrationAddress::parse("sips:account@provider.test.").map_err(failure)?;
    Ok(())
}
