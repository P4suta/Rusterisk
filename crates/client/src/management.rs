use crate::{api, Client, Error};
use std::time::Duration;

const RELOAD_TIMEOUT: Duration = Duration::from_secs(35);

impl Client {
    fn prepare_management_request(
        &self,
        operation: api::ManagementOperation,
    ) -> Result<reqwest::RequestBuilder, Error> {
        let timeout = match operation {
            api::ManagementOperation::ReloadConfiguration => RELOAD_TIMEOUT,
        };
        let metadata = operation.metadata();
        let mut url = self.base.clone();
        url.set_path(metadata.path);
        let method = reqwest::Method::from_bytes(metadata.method.upper().as_bytes())
            .map_err(|_| Error::Invalid("invalid management operation method"))?;
        Ok(self.http.request(method, url).timeout(timeout))
    }

    pub async fn reload_configuration(&self) -> Result<(), Error> {
        let operation = api::ManagementOperation::ReloadConfiguration;
        let response = self.prepare_management_request(operation)?.send().await?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_request_overrides_the_inherited_timeout_without_changing_ari_requests(
    ) -> Result<(), Error> {
        let client = Client::new("http://127.0.0.1:9", "app", "password")?;
        let operation = api::ManagementOperation::ReloadConfiguration;
        let request = client.prepare_management_request(operation)?.build()?;
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
