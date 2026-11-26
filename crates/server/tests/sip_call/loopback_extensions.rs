use super::{
    answer_dialog_at,
    call_paths::{CallPath, Phones},
    exchange, failure, final_response, finish_bye,
    process::{combine, ChildServer},
    start_invitation_as,
    transfer_peer::Peer,
    transfer_signals::acknowledge,
    AudioPeer, BranchNamespace, Invitation,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Response};
use rusterisk_config::{Auth, Config};
use rusterisk_media::{AudioFormat, RtpSender};
use rusterisk_server::ControlError;
use rusterisk_sip::Session;
use rusterisk_test_support::Wait;
use std::{
    fs,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
};

async fn dialog_request(
    phone: &Peer,
    target: SocketAddr,
    answer: &Response,
    method: Method,
    sequence: u32,
) -> Result<(), ControlError> {
    let remote = answer
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let branch = BranchNamespace::random()?.branch(0);
    let packet = format!("{method} {remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n",
        phone.contact()?, answer.from_header().map_err(failure)?.value(),
        answer.to_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value());
    phone
        .signal
        .send_to(packet.as_bytes(), target)
        .await
        .map_err(failure)?;
    Ok(())
}

pub(super) async fn call(
    target: SocketAddr,
    caller: &Peer,
    account: &Auth,
    number: &str,
    callee: &Peer,
    expected_user: &str,
) -> Result<(), ControlError> {
    start_invitation_as(
        &caller.signal,
        target,
        caller.contact()?,
        caller.media()?,
        Invitation {
            extension: number,
            call_id: number,
            from_tag: "calling-phone",
        },
        &account.username,
        account.password.expose(),
    )
    .await
    .map_err(|error| failure(format!("INVITE {number} authentication: {error}")))?;
    let caller_answer = async {
        let answer = final_response(&caller.signal).await?;
        if answer.status_code.code() != 200 {
            return Err(failure(format!(
                "Extension {number} returned {}",
                answer.status_code
            )));
        }
        dialog_request(caller, target, &answer, Method::Ack, 2).await?;
        Ok(answer)
    };
    let callee_answer = async {
        let (media, _, ack) = answer_dialog_at(
            &callee.signal,
            callee.contact()?,
            callee.media()?,
            Some(expected_user),
        )
        .await?;
        acknowledge(&callee.signal, &ack).await?;
        Ok(media)
    };
    let (answer, remote) = tokio::try_join!(caller_answer, callee_answer)
        .map_err(|error| failure(format!("INVITE {number} answer: {error}")))?;
    let session = Session::parse(
        std::str::from_utf8(&answer.body).map_err(failure)?,
        &[rusterisk_config::Codec::Pcmu],
    )
    .map_err(failure)?;
    let mut left = AudioPeer {
        socket: &caller.audio,
        target: session.remote,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [7; 10]).map_err(failure)?,
        sample: 4000,
    };
    let mut right = AudioPeer {
        socket: &callee.audio,
        target: remote,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?,
        sample: -7000,
    };
    exchange(&mut left, &mut right, (-7000, 4000))
        .await
        .map_err(|error| failure(format!("RTP {number}: {error}")))?;
    recover_after_reordered_audio(&mut left, &mut right).await?;
    dialog_request(caller, target, &answer, Method::Bye, 3).await?;
    let (reply, ()) = tokio::try_join!(final_response(&caller.signal), finish_bye(&callee.signal))
        .map_err(|error| failure(format!("BYE {number}: {error}")))?;
    if reply.status_code.code() != 200 {
        return Err(ControlError::Invalid(
            "Expected the caller's hangup to succeed",
        ));
    }
    Ok(())
}

async fn recover_after_reordered_audio(
    left: &mut AudioPeer<'_>,
    right: &mut AudioPeer<'_>,
) -> Result<(), ControlError> {
    let earlier = |peer: &mut AudioPeer<'_>| -> Result<rusterisk_media::EncodedRtp, ControlError> {
        peer.sender
            .packet(rusterisk_media::Frame::new(&[peer.sample; 160], 8000).map_err(failure)?)
            .map_err(failure)?
            .encode()
            .map_err(failure)
    };
    let mut delivery = tokio::time::interval(std::time::Duration::from_millis(30));
    delivery.tick().await;
    let delayed = [earlier(left)?, earlier(right)?];
    left.send().await?;
    right.send().await?;
    delivery.tick().await;
    for (socket, target, packet) in [
        (left.socket, left.target, &delayed[0]),
        (right.socket, right.target, &delayed[1]),
    ] {
        socket
            .data()
            .send_to(packet, target)
            .await
            .map_err(failure)?;
    }
    left.sample = -3000;
    right.sample = 8000;
    exchange(left, right, (8000, -3000)).await
}

async fn exercise(
    phones: Phones,
    config: &Config,
    target: SocketAddr,
    management: SocketAddr,
    path: CallPath,
) -> Result<(), ControlError> {
    phones.exercise(config, target, path).await?;
    let user = config
        .users
        .first()
        .ok_or(ControlError::Invalid("Missing management account"))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Wait::Io.duration())
        .build()
        .map_err(failure)?;
    let channels: Vec<rusterisk_api::Channel> = client
        .get(format!("http://{management}/ari/channels"))
        .basic_auth(&user.name, Some(user.password.expose()))
        .send()
        .await
        .map_err(failure)?
        .error_for_status()
        .map_err(failure)?
        .json()
        .await
        .map_err(failure)?;
    if !channels.is_empty() {
        return Err(ControlError::Invalid("Call channels survived hangup"));
    }
    Ok(())
}

#[tokio::test]
async fn two_registered_phones_exchange_audio_on_the_shipped_extensions() -> Result<(), ControlError>
{
    native(CallPath::RegisteredExtensions).await
}

pub(super) async fn native(path: CallPath) -> Result<(), ControlError> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    run(
        path,
        &source,
        Path::new(env!("CARGO_BIN_EXE_rusterisk")),
        None,
    )
    .await
}

async fn packaged(root: &Path) -> Result<(), ControlError> {
    let bin = root.join("bin");
    let executable = |name: &str| bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let cli = executable("rusterisk-cli");
    run(
        CallPath::RegisteredExtensions,
        &root.join("config"),
        &executable("rusterisk"),
        Some(cli),
    )
    .await
}

async fn run(
    path: CallPath,
    source: &Path,
    binary: &Path,
    cli: Option<std::path::PathBuf>,
) -> Result<(), ControlError> {
    let ip = Ipv4Addr::LOCALHOST.into();
    let phones = Phones::bind(ip, ip).await?;
    let config = Config::load(source).map_err(failure)?;
    let root = tempfile::tempdir().map_err(failure)?;
    let settings = root.path().join("config");
    fs::create_dir(&settings).map_err(failure)?;
    for name in ["ari.conf", "rtp.conf"] {
        fs::copy(source.join(name), settings.join(name)).map_err(failure)?;
    }
    let (sip, extensions) = match path {
        CallPath::RegisteredExtensions => (
            fs::read_to_string(source.join("pjsip.conf")).map_err(failure)?,
            fs::read_to_string(source.join("extensions.conf")).map_err(failure)?,
        ),
        CallPath::IncomingTrunk | CallPath::OutgoingTrunk => phones.settings()?,
    };
    fs::write(settings.join("extensions.conf"), extensions).map_err(failure)?;
    let address = config
        .transports
        .first()
        .ok_or(ControlError::Invalid("Missing SIP listener"))?
        .bind;
    let data = root.path().join("data");
    let mut result = Err(ControlError::Capacity);
    for attempt in 0..4 {
        let reservation = std::net::UdpSocket::bind((address.ip(), 0)).map_err(failure)?;
        let target = reservation.local_addr().map_err(failure)?;
        fs::write(
            settings.join("pjsip.conf"),
            sip.replace(&format!("bind={address}"), &format!("bind={target}")),
        )
        .map_err(failure)?;
        drop(reservation);
        let log = root.path().join(format!("call-{attempt}.log"));
        let started = if cli.is_some() {
            ChildServer::start_from(binary, &settings, &data, log).await
        } else {
            ChildServer::start(&settings, &data, log).await
        };
        let mut server = match started {
            Ok(server) => server,
            Err(error)
                if std::net::UdpSocket::bind(target)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse) =>
            {
                result = Err(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let config = Config::load(&settings).map_err(failure)?;
        let management = server.address();
        let observed = Wait::Scenario
            .bounded(async {
                exercise(phones, &config, target, management, path).await?;
                if let Some(cli) = cli.as_deref() {
                    check_cli(cli, &config, management).await?;
                }
                Ok(())
            })
            .await
            .map_err(failure)
            .and_then(|result| result);
        result = combine(observed, server.terminate().await.map(|_| ()));
        result = combine(result, super::normal_logs::files(root.path(), &config));
        break;
    }
    combine(result, root.close().map_err(failure))
}

#[tokio::test]
#[ignore = "Requires an extracted native server distribution."]
async fn the_packaged_phones_and_cli_complete_the_first_call() -> Result<(), ControlError> {
    let root = std::env::var_os("RUSTERISK_PACKAGED_ROOT")
        .map(std::path::PathBuf::from)
        .ok_or(ControlError::Invalid(
            "An extracted server distribution is required",
        ))?;
    packaged(&root).await
}

async fn check_cli(cli: &Path, config: &Config, address: SocketAddr) -> Result<(), ControlError> {
    use std::process::Stdio;
    use tokio::{io::AsyncWriteExt, process::Command};
    let version = Command::new(cli)
        .arg("--version")
        .kill_on_drop(true)
        .output()
        .await
        .map_err(failure)?;
    if !version.status.success()
        || !version.stdout.starts_with(b"rusterisk-cli ")
        || !version.stderr.is_empty()
    {
        return Err(ControlError::Invalid("Packaged CLI version failed"));
    }
    let user = config
        .users
        .first()
        .ok_or(ControlError::Invalid("Missing management account"))?;
    for command in [
        &["doctor", "extension", "internal", "101"][..],
        &["doctor", "runtime"],
    ] {
        let mut child = Command::new(cli)
            .args(command)
            .args([
                "--server",
                &format!("http://{address}"),
                "--username",
                &user.name,
                "--password-stdin",
                "--json",
            ])
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(failure)?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or(ControlError::Invalid("CLI input is unavailable"))?;
        stdin
            .write_all(user.password.expose().as_bytes())
            .await
            .map_err(failure)?;
        drop(stdin);
        let output = child.wait_with_output().await.map_err(failure)?;
        if !output.status.success() || !output.stderr.is_empty() {
            return Err(failure(format!(
                "Packaged CLI diagnosis failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let sdk = rusterisk_client::Client::new(
            &format!("http://{address}"),
            &user.name,
            user.password.expose(),
        )
        .map_err(failure)?;
        if command.get(1) == Some(&"extension") {
            let route = sdk
                .diagnose_extension("internal", "101")
                .await
                .map_err(failure)?;
            assert_eq!(
                serde_json::from_value::<rusterisk_api::diagnosis::RouteDiagnosis>(report.clone())?,
                route
            );
            assert_eq!(report["destinations"][0]["endpoint"], "alice");
        } else {
            let runtime = sdk.diagnose_runtime().await.map_err(failure)?;
            assert_eq!(
                report["call_slots"],
                serde_json::to_value(runtime.call_slots)?
            );
            assert_eq!(report["call_slots"]["active"], 0);
            assert_eq!(report["call_slots"]["limit"], 64);
        }
    }
    Ok(())
}
