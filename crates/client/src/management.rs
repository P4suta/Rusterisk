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
}

#[cfg(test)]
mod tests {
    use super::*;

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
