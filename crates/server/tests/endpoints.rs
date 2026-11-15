use rusterisk_api::{ChannelId, ChannelState, Endpoint, EndpointState};
use rusterisk_config::Config;
use rusterisk_core::ContactPresence;
use rusterisk_server::{Action, Control, ControlError, EndpointKey, Milestone, Signal};

fn config() -> Result<Config, ControlError> {
    Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[dynamic]\ntype=aor\n[static]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=dynamic\nallow=ulaw\n[phone-extra]\ntype=endpoint\ntransport=udp\nauth=auth\naors=static\nallow=ulaw\n", "", "").map_err(|error| ControlError::Adapter(error.to_string()))
}

fn endpoint(control: &mut Control, resource: &str) -> Result<Endpoint, ControlError> {
    let outcome = control.apply(
        Action::GetEndpoint {
            technology: "PJSIP".into(),
            resource: resource.into(),
        },
        "now",
        0,
    )?;
    assert!(outcome.effects.is_empty());
    assert!(outcome.events.is_empty());
    Ok(serde_json::from_value(
        outcome.body.ok_or(ControlError::Capacity)?,
    )?)
}

#[test]
fn endpoint_queries_keep_contact_availability_and_call_lifetimes_independent(
) -> Result<(), ControlError> {
    let mut control = Control::with_configuration(4, &config()?);
    control.subscribe("app")?;
    assert_eq!(
        endpoint(&mut control, "phone")?.state,
        Some(EndpointState::Offline)
    );
    assert_eq!(
        endpoint(&mut control, "phone-extra")?.state,
        Some(EndpointState::Online)
    );
    let key = control.endpoint_key("phone")?;
    control.signal(
        Signal::EndpointContact {
            key: key.clone(),
            presence: ContactPresence::Present,
        },
        "now",
    )?;
    assert_eq!(
        endpoint(&mut control, "phone")?.state,
        Some(EndpointState::Online)
    );
    for (id, resource) in [("dynamic-call", "phone"), ("static-call", "phone-extra")] {
        control.apply(
            Action::Originate {
                id: ChannelId::try_from(id.to_owned())?,
                endpoint: format!("PJSIP/{resource}"),
                app: "app".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 30,
            },
            "now",
            0,
        )?;
    }
    let call = ChannelId::try_from("dynamic-call".to_owned())?;
    control.signal(
        Signal::Milestone {
            key: control.key(&call)?,
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    assert_eq!(
        endpoint(&mut control, "phone")?.channel_ids,
        std::slice::from_ref(&call)
    );
    control.signal(
        Signal::EndpointContact {
            key: EndpointKey {
                generation: key.generation + 1,
                ..key.clone()
            },
            presence: ContactPresence::Absent,
        },
        "now",
    )?;
    assert_eq!(
        endpoint(&mut control, "phone")?.state,
        Some(EndpointState::Online)
    );
    control.signal(
        Signal::EndpointContact {
            key,
            presence: ContactPresence::Absent,
        },
        "now",
    )?;
    assert_eq!(
        endpoint(&mut control, "phone")?.state,
        Some(EndpointState::Offline)
    );
    assert_eq!(
        endpoint(&mut control, "phone")?.channel_ids,
        std::slice::from_ref(&call)
    );
    assert_eq!(control.channel(&call)?.state, ChannelState::Up);
    let static_key = control.endpoint_key("phone-extra")?;
    control.signal(
        Signal::EndpointContact {
            key: static_key,
            presence: ContactPresence::Absent,
        },
        "now",
    )?;
    assert_eq!(
        endpoint(&mut control, "phone-extra")?.state,
        Some(EndpointState::Online)
    );
    control.apply(Action::Hangup(call), "now", 0)?;
    assert!(endpoint(&mut control, "phone")?.channel_ids.is_empty());
    assert!(matches!(
        endpoint(&mut control, "missing"),
        Err(ControlError::NotFound(_))
    ));
    let listed = control.apply(
        Action::ListEndpoints {
            technology: Some("pjsip".into()),
        },
        "now",
        0,
    )?;
    let listed: Vec<Endpoint> = serde_json::from_value(listed.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].resource, "phone");
    assert_eq!(listed[1].resource, "phone-extra");
    Ok(())
}

#[tokio::test]
async fn endpoint_routes_authenticate_and_return_reference_errors() -> Result<(), ControlError> {
    use axum::{body::Body, http::Request};
    use base64::Engine;
    use http_body_util::BodyExt;
    use rusterisk_server::{http::router, Controller};
    use tower::ServiceExt;

    let (controller, runner, _effects) = Controller::new(config()?, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        for (path, status, message) in [
            ("/ari/endpoints", 200, None),
            ("/ari/endpoints/PJSIP", 200, None),
            ("/ari/endpoints/PJSIP/phone", 200, None),
            (
                "/ari/endpoints/PJSIP/missing",
                404,
                Some("Endpoint not found"),
            ),
            (
                "/ari/endpoints/unknown",
                404,
                Some("No Endpoints found - invalid tech unknown"),
            ),
        ] {
            let request = Request::builder()
                .uri(path)
                .header(
                    "Authorization",
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode("app:test-password")
                    ),
                )
                .body(Body::empty())
                .map_err(|error| ControlError::Adapter(error.to_string()))?;
            let response = app
                .clone()
                .oneshot(request)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?;
            assert_eq!(response.status().as_u16(), status, "{path}");
            let body: serde_json::Value = serde_json::from_slice(
                &response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|error| ControlError::Adapter(error.to_string()))?
                    .to_bytes(),
            )?;
            if let Some(message) = message {
                assert_eq!(body["message"], message);
            } else if path.ends_with("/phone") {
                let endpoint: Endpoint = serde_json::from_value(body)?;
                assert_eq!(endpoint.state, Some(EndpointState::Offline));
                assert!(endpoint.channel_ids.is_empty());
            } else {
                let endpoints: Vec<Endpoint> = serde_json::from_value(body)?;
                assert_eq!(endpoints.len(), 2);
            }
        }
        let request = Request::builder()
            .uri("/ari/endpoints")
            .body(Body::empty())
            .map_err(|error| ControlError::Adapter(error.to_string()))?;
        assert_eq!(
            app.oneshot(request)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .status()
                .as_u16(),
            401
        );
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}
