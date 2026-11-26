use crate::{api, Client, Error};
use std::time::Duration;

const RELOAD_TIMEOUT: Duration = Duration::from_secs(35);

impl Client {
    fn prepare_management_request(
        &self,
        operation: api::ManagementOperation,
        resources: &[&str],
    ) -> Result<reqwest::RequestBuilder, Error> {
        let metadata = operation.metadata();
        let mut url = self.base.clone();
        let mut values = resources.iter().copied();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| Error::Invalid("invalid management origin"))?;
            path.clear();
            for segment in metadata.path.split('/').filter(|s| !s.is_empty()) {
                if segment.starts_with('{') {
                    let value = values
                        .next()
                        .filter(|v| {
                            !v.is_empty()
                                && v.len() <= 256
                                && !matches!(*v, "." | "..")
                                && !v.contains('/')
                                && !v.chars().any(char::is_control)
                        })
                        .ok_or(Error::Invalid("invalid management resource identifier"))?;
                    path.push(value);
                } else {
                    path.push(segment);
                }
            }
        }
        if values.next().is_some() {
            return Err(Error::Invalid("unexpected management resource identifier"));
        }
        let timeout = if operation == api::ManagementOperation::ReloadConfiguration {
            RELOAD_TIMEOUT
        } else {
            Duration::from_secs(10)
        };
        let method = reqwest::Method::from_bytes(metadata.method.upper().as_bytes())
            .map_err(|_| Error::Invalid("invalid management operation method"))?;
        Ok(self.http.request(method, url).timeout(timeout))
    }

    pub async fn capabilities(&self) -> Result<api::capabilities::Capabilities, Error> {
        let response = self
            .prepare_management_request(api::ManagementOperation::Capabilities, &[])?
            .send()
            .await?;
        Self::decode_response(response).await
    }

    pub async fn diagnose_runtime(&self) -> Result<api::diagnosis::RuntimeActivity, Error> {
        let response = self
            .prepare_management_request(api::ManagementOperation::DiagnoseRuntime, &[])?
            .send()
            .await?;
        Self::decode_response(response).await
    }

    pub async fn reload_configuration(&self) -> Result<(), Error> {
        let operation = api::ManagementOperation::ReloadConfiguration;
        let response = self
            .prepare_management_request(operation, &[])?
            .send()
            .await?;
        let status = response.status();
        if status.as_u16() == operation.metadata().success_status || !status.is_success() {
            return Self::decode_response(response).await;
        }
        let message = Self::decode_response::<Option<api::ApiError>>(response)
            .await?
            .unwrap_or_else(|| api::ApiError {
                message: "Configuration reload requires a 204 completion response".to_owned(),
            });
        Err(Error::Api {
            status: status.as_u16(),
            message,
        })
    }

    pub async fn rtp_observation(&self, id: &api::ChannelId) -> Result<api::RtpObservation, Error> {
        let response = self
            .prepare_management_request(
                api::ManagementOperation::ChannelRtpObservation,
                &[id.as_str()],
            )?
            .send()
            .await?;
        let snapshot: api::RtpObservation = Self::decode_response(response).await?;
        if snapshot.channel_id != *id {
            return Err(Error::Invalid("unexpected RTP observation channel"));
        }
        Ok(snapshot)
    }

    pub async fn diagnose_channel(
        &self,
        id: &api::ChannelId,
    ) -> Result<api::diagnosis::ChannelDiagnosis, Error> {
        let response = self
            .prepare_management_request(api::ManagementOperation::DiagnoseChannel, &[id.as_str()])?
            .send()
            .await?;
        let result: api::diagnosis::ChannelDiagnosis = Self::decode_response(response).await?;
        if result.id != *id {
            return Err(Error::Invalid("unexpected diagnosis channel"));
        }
        Ok(result)
    }

    pub async fn diagnose_extension(
        &self,
        context: &str,
        extension: &str,
    ) -> Result<api::diagnosis::RouteDiagnosis, Error> {
        let response = self
            .prepare_management_request(
                api::ManagementOperation::DiagnoseExtension,
                &[context, extension],
            )?
            .send()
            .await?;
        let result: api::diagnosis::RouteDiagnosis = Self::decode_response(response).await?;
        if result.context != context || result.extension != extension {
            return Err(Error::Invalid("unexpected diagnosis location"));
        }
        Ok(result)
    }

    pub async fn test_call(
        &self,
        endpoint: &str,
        deadline_seconds: u16,
    ) -> Result<api::test_call::TestCallResult, Error> {
        if !api::is_valid_resource_id(endpoint) {
            return Err(Error::Invalid("invalid PJSIP test endpoint"));
        }
        if !(1..=api::test_call::MAX_SECONDS).contains(&deadline_seconds) {
            return Err(Error::Invalid("invalid test call deadline"));
        }
        let mut request = self
            .prepare_management_request(api::ManagementOperation::TestCall, &["PJSIP", endpoint])?
            .timeout(Duration::from_secs(u64::from(deadline_seconds) + 10))
            .build()?;
        request
            .url_mut()
            .query_pairs_mut()
            .append_pair("deadline_seconds", &deadline_seconds.to_string());
        Self::decode_response(self.http.execute(request).await?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capabilities_use_the_authenticated_management_route() -> Result<(), Error> {
        use axum::{routing::get, Json, Router};
        use std::future::IntoFuture;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let route = api::ManagementOperation::Capabilities.metadata().path;
        let app = Router::new().route(
            route,
            get(|headers: axum::http::HeaderMap| async move {
                assert!(headers.contains_key(axum::http::header::AUTHORIZATION));
                Json(api::capabilities::Capabilities {
                    reference: api::REFERENCE_VERSION.into(),
                    operations: Vec::new(),
                    verified_call_paths: Vec::from([api::capabilities::VerifiedCallPath {
                        path: "registered_extensions".into(),
                        signaling: "SIP over UDP".into(),
                        media: "RTP".into(),
                        codec: "PCMU".into(),
                        conditions: Vec::from(["Two registered phones".into()]),
                    }]),
                })
            }),
        );
        let task = tokio::spawn(axum::serve(listener, app).into_future());
        let result = Client::new(&format!("http://{address}"), "reader", "password")?
            .capabilities()
            .await;
        task.abort();
        match task.await {
            Err(error) if error.is_cancelled() => {}
            Ok(result) => result?,
            Err(_) => return Err(Error::Closed),
        }
        let result = result?;
        assert_eq!(result.reference, api::REFERENCE_VERSION);
        assert_eq!(result.verified_call_paths[0].path, "registered_extensions");
        Ok(())
    }

    #[tokio::test]
    async fn test_call_posts_to_the_nominated_endpoint_with_its_deadline() -> Result<(), Error> {
        use axum::{extract::OriginalUri, routing::post, Json, Router};
        use std::future::IntoFuture;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = Router::new().route(
            "/rusterisk/test-calls/PJSIP/echo",
            post(
                |uri: OriginalUri, headers: axum::http::HeaderMap| async move {
                    assert_eq!(uri.0.query(), Some("deadline_seconds=17"));
                    assert!(headers.contains_key(axum::http::header::AUTHORIZATION));
                    Json(serde_json::json!({
                        "channel_id":"probe-42","accepted":true,"answered":true,
                        "signal_verified":true,"outcome":"verified"
                    }))
                },
            ),
        );
        let task = tokio::spawn(axum::serve(listener, server).into_future());
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let result = client.test_call("echo", 17).await;
        task.abort();
        match task.await {
            Err(error) if error.is_cancelled() => {}
            Ok(result) => result?,
            Err(_) => return Err(Error::Closed),
        }
        let result = result?;
        assert_eq!(result.channel_id.as_str(), "probe-42");
        assert_eq!(result.outcome, api::test_call::TestCallOutcome::Verified);
        for invalid in [0, api::test_call::MAX_SECONDS + 1] {
            assert!(matches!(
                client.test_call("echo", invalid).await,
                Err(Error::Invalid(_))
            ));
        }
        assert!(matches!(
            client.test_call("../desk", 10).await,
            Err(Error::Invalid(_))
        ));
        Ok(())
    }

    #[test]
    fn reload_request_overrides_the_inherited_timeout_without_changing_ari_requests(
    ) -> Result<(), Error> {
        let client = Client::new("http://127.0.0.1:9", "app", "password")?;
        let operation = api::ManagementOperation::ReloadConfiguration;
        let request = client.prepare_management_request(operation, &[])?.build()?;
        assert_eq!(request.timeout(), Some(&Duration::from_secs(35)));
        assert_eq!(
            request.method().as_str(),
            operation.metadata().method.upper()
        );
        assert_eq!(request.url().path(), operation.metadata().path);
        assert!(request.url().query().is_none());
        assert!(request.body().is_none());
        let ordinary = client.prepare_request("asterisk.ping", &[], &[])?.build()?;
        assert!(ordinary.timeout().is_none());
        assert_eq!(ordinary.url().path(), "/ari/asterisk/ping");
        Ok(())
    }
}
