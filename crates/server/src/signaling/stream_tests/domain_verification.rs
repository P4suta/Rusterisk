use super::{
    configuration, credentials, failure, initial_call, Action, ChannelId, ControlError, Controller,
    MediaHub, Signaling, TlsContext, TlsFiles,
};
use rusterisk_test_support::Wait;
use tokio::net::TcpListener;

#[tokio::test]
async fn initial_tls_invites_authenticate_uri_only_domain_certificates() -> Result<(), ControlError>
{
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut params = rcgen::CertificateParams::default();
    params.subject_alt_names = std::iter::once(rcgen::SanType::URI(
        "sip:localhost".try_into().map_err(failure)?,
    ))
    .collect();
    let key = rcgen::KeyPair::generate().map_err(failure)?;
    let identity = params.self_signed(&key).map_err(failure)?;
    let certificate = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    std::fs::write(&certificate, identity.pem()).map_err(failure)?;
    std::fs::write(&private_key, key.serialize_pem()).map_err(failure)?;
    let files = format!(
        "cert_file={}\npriv_key_file={}\nca_list_file={}\n",
        certificate.display(),
        private_key.display(),
        certificate.display()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let target = format!("sip:phone@localhost:{}", address.port());
    let mut config = configuration("tls", &files)?;
    config.aors[0].contact = Some(target.clone());
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result: Result<(), ControlError> = async {
        let subscription = controller
            .subscribe(credentials()?, None, "app".into())
            .await?;
        let id = ChannelId::try_from("uri-only-domain".to_owned())?;
        controller
            .execute(
                credentials()?,
                None,
                Action::Originate {
                    id: id.clone(),
                    endpoint: "PJSIP/phone".into(),
                    app: "app".into(),
                    args: Vec::new(),
                    caller: "100".into(),
                    timeout: 30,
                },
            )
            .await?;
        let (socket, _) = Wait::Ready
            .bounded(listener.accept())
            .await
            .map_err(failure)?
            .map_err(failure)?;
        let context = TlsContext::load(&TlsFiles {
            certificate: certificate.to_string_lossy().into(),
            private_key: private_key.to_string_lossy().into(),
        })
        .await?;
        let socket = context.accept(socket).await?;
        initial_call(socket, address, "TLS", &controller, &id, &target).await?;
        drop(subscription);
        Ok(())
    }
    .await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
