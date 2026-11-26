use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{Application, ChannelId};
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, ControlError, Controller, Credentials, IncomingCall, IncomingTarget,
};
use std::collections::{BTreeMap, BTreeSet};
use tower::ServiceExt;

#[tokio::test]
async fn every_declared_operation_authenticates_and_denies_read_only_writes(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    let result = async {
        let ari = rusterisk_api::OPERATIONS.iter().map(|op| ("/ari", op));
        let management = rusterisk_api::MANAGEMENT_OPERATIONS
            .iter()
            .map(|op| ("", op.metadata()));
        for (prefix, op) in ari.chain(management) {
            let path = op
                .path
                .split('/')
                .map(|part| if part.starts_with('{') { "probe" } else { part })
                .collect::<Vec<_>>()
                .join("/");
            let path = format!("{prefix}{path}");
            for (user, status) in [
                (None, StatusCode::UNAUTHORIZED),
                (Some("reader"), StatusCode::FORBIDDEN),
            ] {
                if user.is_some() && op.method == rusterisk_api::Method::Get {
                    continue;
                }
                let response = app
                    .clone()
                    .oneshot(request(op.method.upper(), &path, user)?)
                    .await
                    .unwrap_or_else(|error| match error {});
                assert_eq!(response.status(), status, "{}", op.id);
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|error| ControlError::Adapter(error.to_string()))?
                    .to_bytes();
                let error: rusterisk_api::ApiError = serde_json::from_slice(&bytes)?;
                assert!(!error.message.is_empty(), "{}", op.id);
            }
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}

#[tokio::test]
async fn published_paths_report_methods_and_unavailable_operations_through_http(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 4);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    let mut paths: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut unavailable = 0;
    for row in include_str!("../../api/tests/fixtures/asterisk-22.9.0-routes.tsv").lines() {
        let mut fields = row.split('\t');
        let method = fields
            .next()
            .ok_or(ControlError::Invalid("Missing fixture method"))?;
        let template = fields
            .next()
            .ok_or(ControlError::Invalid("Missing fixture path"))?;
        let concrete = template
            .split('/')
            .map(|part| if part.starts_with('{') { "probe" } else { part })
            .collect::<Vec<_>>()
            .join("/");
        let uri = format!("/ari{concrete}");
        paths.entry(uri.clone()).or_default().insert(method);
        if rusterisk_api::OPERATIONS
            .iter()
            .any(|op| op.method.upper() == method && op.path == template)
        {
            continue;
        }
        unavailable += 1;
        for (user, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (
                Some("reader"),
                if method == "GET" {
                    StatusCode::NOT_IMPLEMENTED
                } else {
                    StatusCode::FORBIDDEN
                },
            ),
            (Some("test"), StatusCode::NOT_IMPLEMENTED),
        ] {
            let reply = app
                .clone()
                .oneshot(request(method, &uri, user)?)
                .await
                .unwrap_or_else(|error| match error {});
            assert_eq!(reply.status(), expected, "{method} {uri} {user:?}");
            if expected == StatusCode::NOT_IMPLEMENTED {
                let bytes = reply
                    .into_body()
                    .collect()
                    .await
                    .map_err(|error| ControlError::Adapter(error.to_string()))?
                    .to_bytes();
                assert_eq!(
                    serde_json::from_slice::<rusterisk_api::ApiError>(&bytes)?.message,
                    "Operation not supported"
                );
            }
        }
    }
    assert_eq!(unavailable, 38);
    assert_eq!(paths.len(), 72);
    for (uri, methods) in paths {
        let allow = ["OPTIONS", "GET", "POST", "PUT", "DELETE"]
            .into_iter()
            .filter(|method| {
                *method == "OPTIONS" || (uri != "/ari/events" && methods.contains(method))
            })
            .collect::<Vec<_>>()
            .join(",");
        for (method, status) in [
            ("OPTIONS", StatusCode::NO_CONTENT),
            ("PATCH", StatusCode::METHOD_NOT_ALLOWED),
        ] {
            let reply = app
                .clone()
                .oneshot(request(method, &uri, Some("test"))?)
                .await
                .unwrap_or_else(|error| match error {});
            assert_eq!(reply.status(), status, "{method} {uri}");
            assert_eq!(
                reply
                    .headers()
                    .get("allow")
                    .and_then(|value| value.to_str().ok()),
                Some(allow.as_str()),
                "{method} {uri}"
            );
        }
    }
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

#[tokio::test]
async fn ari_routing_distinguishes_unknown_methods_and_unavailable_adapters(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    for (method, path, status, allow) in [
        ("GET", "/ari/asterisk/modules", 501, None),
        ("GET", "/ari/channels/user/rtp_statistics", 501, None),
        ("PUT", "/ari/deviceStates/test", 501, None),
        ("POST", "/ari/channels/create/answer", 404, None),
        (
            "DELETE",
            "/ari/channels/%63reate",
            405,
            Some("OPTIONS,POST"),
        ),
        ("GET", "/ari/%61sterisk/ping", 200, None),
        (
            "DELETE",
            "/ari/channels/externalMedia",
            405,
            Some("OPTIONS,POST"),
        ),
        ("OPTIONS", "/ari/channels/create", 204, Some("OPTIONS,POST")),
        ("OPTIONS", "/ari/events", 204, Some("OPTIONS")),
        (
            "OPTIONS",
            "/ari/channels/user",
            204,
            Some("OPTIONS,GET,POST,DELETE"),
        ),
        ("OPTIONS", "/ari/missing", 404, None),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, path, Some("test"))?)
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(response.status().as_u16(), status, "{method} {path}");
        assert_eq!(
            response
                .headers()
                .get("allow")
                .and_then(|value| value.to_str().ok()),
            allow,
            "{method} {path}"
        );
    }
    let response = app
        .oneshot(request("GET", "/ari/channels/", Some("test"))?)
        .await
        .unwrap_or_else(|error| match error {});
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .to_bytes();
    assert_eq!(
        serde_json::from_slice::<rusterisk_api::ApiError>(&body)?.message,
        "ARI URLs do not end with a slash. Try /ari/channels"
    );
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

fn config() -> Result<Config, ControlError> {
    Config::parse("[general]\nbindaddr=127.0.0.1\n", "[general]\nallowed_origins=https://localhost\n[test]\ntype=user\npassword=password\n[reader]\ntype=user\npassword=password\nread_only=yes\n", "", "", "").map_err(|error| ControlError::Adapter(error.to_string()))
}

fn request(method: &str, path: &str, user: Option<&str>) -> Result<Request<Body>, ControlError> {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(user) = user {
        request = request.header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:password"))
            ),
        );
    }
    request
        .body(Body::empty())
        .map_err(|error| ControlError::Adapter(error.to_string()))
}

#[tokio::test]
async fn app_queries_require_authentication_and_follow_subscription_lifetimes(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        assert_eq!(
            app.clone()
                .oneshot(request("GET", "/ari/applications", None)?)
                .await
                .unwrap_or_else(|error| match error {})
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let subscription = controller
            .subscribe(
                Credentials::new("test".into(), "password".into())?,
                None,
                "phones".into(),
            )
            .await?;
        let listed = app
            .clone()
            .oneshot(request("GET", "/ari/applications", Some("reader"))?)
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(listed.status(), StatusCode::OK);
        let listed: Vec<Application> = serde_json::from_slice(
            &listed
                .into_body()
                .collect()
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .to_bytes(),
        )?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "phones");
        let id = ChannelId::try_from("incoming".to_owned())?;
        controller
            .incoming(IncomingCall {
                id: id.clone(),
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "phones".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
        let detail = app
            .clone()
            .oneshot(request("GET", "/ari/applications/phones", Some("reader"))?)
            .await
            .unwrap_or_else(|error| match error {});
        assert_eq!(detail.status(), StatusCode::OK);
        let detail: Application = serde_json::from_slice(
            &detail
                .into_body()
                .collect()
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .to_bytes(),
        )?;
        assert_eq!(detail.channel_ids, [id]);
        assert_eq!(
            app.clone()
                .oneshot(request("GET", "/ari/applications/missing", Some("reader"))?)
                .await
                .unwrap_or_else(|error| match error {})
                .status(),
            StatusCode::NOT_FOUND
        );
        drop(subscription);
        assert_eq!(
            app.oneshot(request("GET", "/ari/applications/phones", Some("reader"))?)
                .await
                .unwrap_or_else(|error| match error {})
                .status(),
            StatusCode::NOT_FOUND
        );
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}

#[tokio::test]
async fn discovery_documents_use_the_requested_authority_and_refuse_invalid_hosts(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 1);
    let task = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        for (path, version, base) in [
            ("/ari/api-docs/resources.json", "1.1", "/ari"),
            ("/ari/api-docs/channels.json", "1.1", "/ari"),
            ("/rusterisk/api-docs/resources.json", "1.2", "/rusterisk"),
            ("/rusterisk/api-docs/channels.json", "1.2", "/ari"),
        ] {
            let mut request = request("GET", path, Some("test"))?;
            request.headers_mut().insert(
                "Host",
                "pbx.example:8088".parse().map_err(
                    |error: axum::http::header::InvalidHeaderValue| {
                        ControlError::Adapter(error.to_string())
                    },
                )?,
            );
            let response = app
                .clone()
                .oneshot(request)
                .await
                .unwrap_or_else(|error| match error {});
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .to_bytes();
            let document: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                document["basePath"],
                format!("http://pbx.example:8088{base}")
            );
            assert_eq!(document["swaggerVersion"], version);
        }
        for host in ["", "person@pbx.example", "pbx.example/path"] {
            let mut request = request("GET", "/ari/api-docs/resources.json", Some("test"))?;
            request.headers_mut().insert(
                "Host",
                host.parse()
                    .map_err(|error: axum::http::header::InvalidHeaderValue| {
                        ControlError::Adapter(error.to_string())
                    })?,
            );
            assert_eq!(
                app.clone()
                    .oneshot(request)
                    .await
                    .unwrap_or_else(|error| match error {})
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}

#[tokio::test]
async fn authentication_origin_and_read_only_permissions_apply_to_all_resources(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/ari/channels", None)?)
            .await
            .unwrap_or_else(|error| match error {})
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/ari/channels", Some("reader"))?)
            .await
            .unwrap_or_else(|error| match error {})
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/ari/bridges?bridgeId=mix",
                Some("reader")
            )?)
            .await
            .unwrap_or_else(|error| match error {})
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut wrong_origin = request("GET", "/ari/channels", Some("test"))?;
    wrong_origin.headers_mut().insert(
        "Origin",
        "https://untrusted.example".parse().map_err(
            |error: axum::http::header::InvalidHeaderValue| {
                ControlError::Adapter(error.to_string())
            },
        )?,
    );
    assert_eq!(
        app.oneshot(wrong_origin)
            .await
            .unwrap_or_else(|error| match error {})
            .status(),
        StatusCode::FORBIDDEN
    );
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

#[tokio::test]
async fn unavailable_bridge_features_preserve_membership_until_an_ordinary_add(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    let _subscription = controller
        .subscribe(
            Credentials::new("test".into(), "password".into())?,
            None,
            "phones".into(),
        )
        .await?;
    let channel = ChannelId::try_from("incoming".to_owned())?;
    let key = controller
        .incoming(IncomingCall {
            id: channel.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: IncomingTarget::Stasis {
                application: "phones".into(),
                arguments: Vec::new(),
            },
        })
        .await?;
    controller
        .signal(rusterisk_server::Signal::Milestone {
            key,
            milestone: rusterisk_server::Milestone::Answered,
        })
        .await?;
    let route = "/ari/bridges/mix/addChannel?channel=incoming";
    let send = |req| app.clone().oneshot(req);
    let created = send(request("POST", "/ari/bridges?bridgeId=mix", Some("test"))?)
        .await
        .unwrap_or_else(|e| match e {});
    assert_eq!(created.status(), StatusCode::OK);
    for (query, body) in [
        ("&mute=true", ""),
        ("", r#"{"absorbDTMF":true}"#),
        ("&inhibitConnectedLineUpdates=yes", ""),
        ("", r#"{"role":"announcer"}"#),
    ] {
        let mut req = request("POST", &format!("{route}{query}"), Some("test"))?;
        *req.body_mut() = Body::from(body);
        let response = send(req).await.unwrap_or_else(|e| match e {});
        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{query} {body}"
        );
    }
    for added in [false, true] {
        if added {
            let mut req = request(
                "POST",
                &format!("{route}&mute=true&absorbDTMF=no"),
                Some("test"),
            )?;
            *req.body_mut() = Body::from(r#"{"mute":false,"inhibitConnectedLineUpdates":false}"#);
            assert_eq!(
                send(req).await.unwrap_or_else(|e| match e {}).status(),
                StatusCode::NO_CONTENT
            );
        }
        let response = send(request("GET", "/ari/bridges/mix", Some("test"))?)
            .await
            .unwrap_or_else(|e| match e {});
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .to_bytes();
        let bridge: rusterisk_api::Bridge = serde_json::from_slice(&bytes)?;
        assert_eq!(
            bridge.channels.as_slice(),
            if added {
                std::slice::from_ref(&channel)
            } else {
                &[]
            }
        );
    }
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

#[tokio::test]
async fn http_bridge_lifecycle_and_known_unsupported_operations_have_ari_errors(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    for (path, body) in [
        ("/ari/bridges?bridgeId=staging&type=holding", ""),
        (
            "/ari/bridges/staging?type=mixing",
            r#"{"type":"mixing,video_sfu"}"#,
        ),
    ] {
        let mut req = request("POST", path, Some("test"))?;
        *req.body_mut() = Body::from(body);
        assert_eq!(
            app.clone()
                .oneshot(req)
                .await
                .unwrap_or_else(|e| match e {})
                .status(),
            StatusCode::NOT_IMPLEMENTED
        );
        let read = request("GET", "/ari/bridges/staging", Some("test"))?;
        assert_eq!(
            app.clone()
                .oneshot(read)
                .await
                .unwrap_or_else(|e| match e {})
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/ari/bridges?bridgeId=mix&name=Conversation",
            Some("test"),
        )?)
        .await
        .unwrap_or_else(|error| match error {});
    assert_eq!(created.status(), StatusCode::OK);
    let bytes = created
        .into_body()
        .collect()
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .to_bytes();
    let bridge: rusterisk_api::Bridge = serde_json::from_slice(&bytes)?;
    assert_eq!(bridge.id.as_str(), "mix");
    assert_eq!(
        app.clone()
            .oneshot(request("DELETE", "/ari/bridges/mix", Some("test"))?)
            .await
            .unwrap_or_else(|error| match error {})
            .status(),
        StatusCode::NO_CONTENT
    );
    let missing = app
        .clone()
        .oneshot(request("GET", "/ari/bridges/mix", Some("test"))?)
        .await
        .unwrap_or_else(|error| match error {});
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let bytes = missing
        .into_body()
        .collect()
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .to_bytes();
    let error: rusterisk_api::ApiError = serde_json::from_slice(&bytes)?;
    assert_eq!(error.message, "Bridge not found");
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/ari/channels/externalMedia",
                Some("test")
            )?)
            .await
            .unwrap_or_else(|error| match error {})
            .status(),
        StatusCode::BAD_REQUEST
    );
    for path in ["/ari/channels/x/snoop", "/ari/channels/x/snoop/observer"] {
        assert_eq!(
            app.clone()
                .oneshot(request("POST", path, Some("test"))?)
                .await
                .unwrap_or_else(|error| match error {})
                .status(),
            StatusCode::NOT_IMPLEMENTED,
            "{path}"
        );
    }
    for path in [
        "/ari/unknown",
        "/ari/channels/x/unknown",
        "/ari/channels/x/play/extra/unexpected",
    ] {
        assert_eq!(
            app.clone()
                .oneshot(request("POST", path, Some("test"))?)
                .await
                .unwrap_or_else(|error| match error {})
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}
