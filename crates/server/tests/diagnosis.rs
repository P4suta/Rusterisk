use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, Action, ControlError, Controller, Credentials, IncomingCall, IncomingTarget,
    Milestone, Signal,
};
use rusterisk_test_support::Wait;
use serde_json::Value;
use tower::ServiceExt;

fn fail(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn read(app: &Router, path: &str) -> Result<Value, ControlError> {
    let req = Request::builder()
        .uri(path)
        .header("Authorization", "Basic cmVhZGVyOnBhc3N3b3Jk")
        .body(Body::empty())
        .map_err(fail)?;
    let response = app
        .clone()
        .oneshot(req)
        .await
        .unwrap_or_else(|never| match never {});
    assert_eq!(response.status().as_u16(), 200, "{path}");
    Ok(serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .map_err(fail)?
            .to_bytes(),
    )?)
}

async fn read_after_signal(
    app: &Router,
    path: &str,
    applied: impl Fn(&Value) -> bool,
) -> Result<Value, ControlError> {
    Wait::Ready
        .bounded(async {
            loop {
                let observed = read(app, path).await?;
                if applied(&observed) {
                    return Ok(observed);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| ControlError::Adapter(format!("Signal did not reach {path}")))?
}

#[tokio::test]
async fn diagnosis_follows_dialplan_and_live_call_identity_with_read_only_credentials(
) -> Result<(), ControlError> {
    let config=Config::parse("[general]\nbindaddr=127.0.0.1\n","[app]\ntype=user\npassword=write\n[reader]\ntype=user\nread_only=yes\npassword=password\n",
      "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=local\n[a]\ntype=aor\ncontact=sip:p@127.0.0.1:5061\n[b]\ntype=aor\n[desk]\ntype=endpoint\ntransport=udp\nauth=auth\naors=a\nallow=ulaw\n[mobile]\ntype=endpoint\ntransport=udp\nauth=auth\naors=b\nallow=ulaw\n",
      "","[home]\nexten => _2XX,1,Goto(route,300,1)\nexten => 201,1,Stasis(phone)\n[route]\nexten => 300,1,Dial(PJSIP/desk&PJSIP/mobile)\n").map_err(fail)?;
    let (controller, runner, _effects) = Controller::new(config, 8);
    let task = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        let capabilities = read(&app, "/rusterisk/capabilities").await?;
        assert_eq!(capabilities["reference"], rusterisk_api::REFERENCE_VERSION);
        let verified = capabilities["verified_call_paths"]
            .as_array()
            .ok_or(ControlError::Invalid("Missing call paths"))?;
        let browser = verified
            .iter()
            .find(|profile| profile["path"] == "webrtc_application")
            .ok_or(ControlError::Invalid("Missing verified browser call"))?;
        assert_eq!(browser["codec"], "PCMU");
        assert_eq!(browser["media"], "DTLS-SRTP");
        assert_eq!(browser["signaling"], "SIP over WebSocket");
        assert!(browser["conditions"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        let operations = capabilities["operations"]
            .as_array()
            .ok_or(ControlError::Invalid("Missing operation capabilities"))?;
        assert_eq!(operations.len(), rusterisk_api::OPERATIONS.len());
        for (published, declared) in operations.iter().zip(rusterisk_api::OPERATIONS) {
            assert_eq!(published["operation"], declared.id);
            assert!(published["call_verified"].is_boolean());
        }
        let path = "/rusterisk/diagnostics/runtime";
        let request = Request::builder()
            .uri(path)
            .body(Body::empty())
            .map_err(fail)?;
        let denied = app
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});
        assert_eq!(denied.status().as_u16(), 401);
        let first = read(&app, path).await?;
        let second = read(&app, path).await?;
        assert_eq!(
            second["call_slots"],
            serde_json::json!({"active":0,"awaiting_release":0,"limit":8,"capacity_refusals":0})
        );
        assert!(second["control"]["turns"].as_u64() > first["control"]["turns"].as_u64());
        for name in ["control", "signaling", "media"] {
            for counter in ["turns", "late_turns", "drops"] {
                assert!(second[name][counter].is_u64());
            }
        }
        let owner = Credentials::new("app".into(), "write".into())?;
        let _phone = controller
            .subscribe(owner.clone(), None, "phone".into())
            .await?;
        for name in ["first-leg", "second-leg"] {
            let key = controller
                .incoming(IncomingCall {
                    id: ChannelId::try_from(name.to_owned())?,
                    endpoint: "desk".into(),
                    caller: "432".into(),
                    context: "home".into(),
                    extension: "201".into(),
                    target: IncomingTarget::Stasis {
                        application: "phone".into(),
                        arguments: Vec::new(),
                    },
                })
                .await?;
            controller
                .signal(Signal::Milestone {
                    key: key.clone(),
                    milestone: Milestone::Answered,
                })
                .await?;
            read_after_signal(
                &app,
                &format!("/rusterisk/diagnostics/channels/{name}"),
                |channel| channel["state"] == "Up",
            )
            .await?;
            if name == "first-leg" {
                controller
                    .execute(owner.clone(), None, Action::Hold(key.id))
                    .await?;
            }
        }
        let key = rusterisk_server::EndpointKey {
            resource: "mobile".into(),
            generation: 1,
        };
        for (presence, expected) in [
            (rusterisk_core::ContactPresence::Present, "present"),
            (rusterisk_core::ContactPresence::Absent, "absent"),
        ] {
            controller
                .signal(Signal::EndpointContact {
                    key: key.clone(),
                    presence,
                })
                .await?;
            read_after_signal(
                &app,
                "/rusterisk/diagnostics/extensions/home/234",
                |route| route["destinations"][1]["registration"] == expected,
            )
            .await?;
        }
        assert_eq!(
            read(&app, "/rusterisk/diagnostics/extensions/home/999").await?["disposition"],
            "missing"
        );
        let route = read(&app, "/rusterisk/diagnostics/extensions/home/234").await?;
        assert_eq!(route["steps"][0]["pattern"], "_2XX");
        assert_eq!(route["disposition"], "dial");
        assert_eq!(route["destinations"][0]["endpoint"], "desk");
        assert_eq!(route["destinations"][0]["static_contact"], true);
        assert_eq!(route["destinations"][1]["registration"], "absent");
        assert_eq!(route["destinations"][1]["available"], false);
        let handoff = read(&app, "/rusterisk/diagnostics/extensions/home/201").await?;
        assert_eq!(handoff["application"], "phone");
        assert_eq!(handoff["channels"].as_array().map(Vec::len), Some(2));
        for (name, held) in [("first-leg", true), ("second-leg", false)] {
            let state = read(&app, &format!("/rusterisk/diagnostics/channels/{name}")).await?;
            assert_eq!(state["held"], held);
            assert_eq!(state["state"], "Up");
            assert_eq!(state["endpoint"], "desk");
            assert_eq!(state["id"], name);
        }
        let initial = read(&app, "/rusterisk/diagnostics/channels/second-leg").await?;
        let key = rusterisk_server::CallKey {
            id: ChannelId::try_from("second-leg".to_owned())?,
            generation: initial["generation"]
                .as_u64()
                .ok_or(ControlError::Capacity)?,
        };
        controller
            .signal(Signal::PeerDirection {
                key,
                direction: rusterisk_core::PeerMediaDirection::SendOnly,
            })
            .await?;
        let remote_hold = read_after_signal(
            &app,
            "/rusterisk/diagnostics/channels/second-leg",
            |channel| channel["hold_source"] == "peer",
        )
        .await?;
        assert_eq!(remote_hold["held"], true);
        assert_eq!(remote_hold["hold_source"], "peer");
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(fail)??;
    result
}
