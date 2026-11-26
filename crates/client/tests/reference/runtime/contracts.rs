use super::{combine, Fixture};
use crate::{music_on_hold, sounds, Client, Error};
use rusterisk_client::api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::storage::StorageWorker;
use rusterisk_server::test_time::Wait;
use std::{
    future::Future,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

const MUSIC: &str =
    "[default]\nmode=files\ndirectory=moh/default\n[Main]\nmode=files\ndirectory=moh/main\n";
const MARKER: &[u8] = b"Borrowed reference configuration.\n";

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    condition.then_some(()).ok_or(Error::Invalid(message))
}

fn merged(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; runtime contract cleanup failed: {cleanup}"
        ))),
    }
}

async fn bounded<T>(future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    Wait::Ready
        .bounded(future)
        .await
        .map_err(|_| Error::Timeout)?
}

fn wave(sample: i16) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16_044);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&16_036_u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    for _ in 0..8000 {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush().map_err(Error::from)
}

struct Inputs {
    root: PathBuf,
}

impl Inputs {
    fn new() -> Result<Self, Error> {
        let root = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "rusterisk-reference-runtime-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root)?;
        let inputs = Self { root };
        let prepared = (|| {
            std::fs::create_dir_all(inputs.root.join("native/config"))?;
            for class in ["default", "main"] {
                std::fs::create_dir_all(inputs.data().join("moh").join(class))?;
            }
            write(&inputs.root.join("borrowed-marker.txt"), MARKER)?;
            write(
                &inputs.root.join("native/config/musiconhold.conf"),
                MUSIC.as_bytes(),
            )?;
            write(&inputs.data().join("moh/default/known.wav"), &wave(1200))?;
            write(&inputs.data().join("moh/main/known.wav"), &wave(2400))
        })();
        match prepared {
            Ok(()) => Ok(inputs),
            Err(primary) => {
                let fallback = primary.to_string();
                match inputs.close(Err(primary)) {
                    Err(error) => Err(error),
                    Ok(()) => Err(failure(fallback)),
                }
            }
        }
    }

    fn data(&self) -> PathBuf {
        self.root.join("native/data")
    }

    fn configuration(&self) -> Result<Config, Error> {
        let mut config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test-password\n",
            "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
            "",
            "",
        )
        .map_err(failure)?;
        config.http.bind.set_port(0);
        Ok(config)
    }

    fn preserved(&self) -> Result<(), Error> {
        require(
            self.root.is_dir(),
            "Runtime removed the borrowed configuration root",
        )?;
        for (path, expected) in [
            (self.root.join("borrowed-marker.txt"), MARKER.to_vec()),
            (
                self.root.join("native/config/musiconhold.conf"),
                MUSIC.as_bytes().to_vec(),
            ),
            (self.data().join("moh/default/known.wav"), wave(1200)),
            (self.data().join("moh/main/known.wav"), wave(2400)),
        ] {
            require(
                std::fs::read(path)? == expected,
                "Runtime changed a borrowed music or configuration asset",
            )?;
        }
        Ok(())
    }

    fn close(self, primary: Result<(), Error>) -> Result<(), Error> {
        merged(
            primary,
            std::fs::remove_dir_all(self.root).map_err(Error::from),
        )
    }
}

async fn store_reopened(data: &Path) -> Result<(), Error> {
    let (storage, worker) = StorageWorker::start(data.to_owned())
        .await
        .map_err(failure)?;
    let primary = bounded(async {
        require(
            storage.recordings().await.map_err(failure)?.is_empty(),
            "Runtime retained an unexpected fixture recording",
        )
    })
    .await;
    let cleanup = worker.close().await.map_err(failure);
    drop(storage);
    merged(primary, cleanup)
}

async fn released(
    http: SocketAddr,
    listeners: &[(String, SocketAddr)],
    data: &Path,
) -> Result<(), Error> {
    let mut result = bounded(async {
        tokio::net::TcpListener::bind(http)
            .await
            .map(drop)
            .map_err(Error::from)
    })
    .await;
    result = merged(
        result,
        require(
            listeners.len() == 1,
            "Runtime fixture changed the requested SIP listener inventory",
        ),
    );
    for (_, address) in listeners {
        result = merged(
            result,
            bounded(async {
                tokio::net::UdpSocket::bind(address)
                    .await
                    .map(drop)
                    .map_err(Error::from)
            })
            .await,
        );
    }
    merged(result, store_reopened(data).await)
}

async fn native_music(client: &Client) -> Result<(), Error> {
    bounded(client.ping()).await?;
    let app = ChannelId::try_from("music-runtime-contract".to_owned())?;
    let mut session = bounded(client.subscribe(&app)).await?;
    let primary = music_on_hold::compare(
        client,
        &mut session,
        app.as_str(),
        Ipv4Addr::LOCALHOST,
        music_on_hold::ComparisonServer::Rusterisk,
    )
    .await;
    merged(primary, bounded(session.shutdown()).await)
}

#[tokio::test]
async fn sound_preparation_uses_explicit_native_data_and_closes_its_store() -> Result<(), Error> {
    let inputs = Inputs::new()?;
    let primary = async {
        let data = inputs.data();
        let (storage, worker) = sounds::prepare(&inputs.root, &data).await?;
        let observed = (|| {
            require(
                data.join("sounds/en/digits/rusterisk-one.wav").is_file(),
                "Sound preparation wrote outside explicit native data",
            )?;
            require(
                data.join("sounds/ja/digits/rusterisk-one.wav").is_file(),
                "Sound preparation lost its alternate language",
            )?;
            require(
                data.join("sounds/en/rusterisk-position.wav").is_file(),
                "Sound preparation omitted the interruption fixture",
            )?;
            require(
                !inputs.root.join("native-data").exists(),
                "Sound preparation created the legacy implicit data path",
            )
        })();
        let cleanup = worker.close().await.map_err(failure);
        drop(storage);
        let result = merged(observed, cleanup);
        let result = merged(result, store_reopened(&data).await);
        merged(result, inputs.preserved())
    }
    .await;
    inputs.close(primary)
}

#[tokio::test]
async fn owned_runtime_fixture_runs_the_native_music_comparison_and_releases_resources(
) -> Result<(), Error> {
    let inputs = Inputs::new()?;
    let primary = async {
        let fixture = Fixture::start(inputs.configuration()?, &inputs.root).await?;
        let http = fixture.address();
        let listeners = fixture.sip_listeners().to_owned();
        let primary = native_music(fixture.client()).await;
        let primary = fixture.finish(primary).await;
        let primary = merged(primary, released(http, &listeners, &inputs.data()).await);
        merged(primary, inputs.preserved())
    }
    .await;
    inputs.close(primary)
}

#[tokio::test]
async fn a_failed_comparison_still_releases_runtime_storage_and_borrowed_configuration(
) -> Result<(), Error> {
    let inputs = Inputs::new()?;
    let primary = async {
        let fixture = Fixture::start(inputs.configuration()?, &inputs.root).await?;
        let http = fixture.address();
        let listeners = fixture.sip_listeners().to_owned();
        let primary = bounded(fixture.client().ping())
            .await
            .and(Err(Error::Invalid("requested runtime comparison failure")));
        let finished = fixture.finish(primary).await;
        let primary = match finished {
            Err(Error::Invalid("requested runtime comparison failure")) => Ok(()),
            Err(error) => Err(error),
            Ok(()) => Err(Error::Invalid(
                "Runtime fixture discarded its primary comparison failure",
            )),
        };
        let primary = merged(primary, released(http, &listeners, &inputs.data()).await);
        merged(primary, inputs.preserved())
    }
    .await;
    inputs.close(primary)
}

#[test]
fn runtime_cleanup_keeps_primary_and_cleanup_failures_identifiable() -> Result<(), Error> {
    let result = combine(
        Err(Error::Invalid("primary-runtime-contract")),
        Err(Error::Invalid("cleanup-runtime-contract")),
    );
    let Err(Error::Cleanup(message)) = result else {
        return Err(Error::Invalid(
            "Runtime cleanup discarded simultaneous failures",
        ));
    };
    require(
        message.contains("primary-runtime-contract")
            && message.contains("cleanup-runtime-contract"),
        "Runtime cleanup diagnostics lost a failure",
    )
}
