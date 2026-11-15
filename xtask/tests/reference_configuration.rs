use std::{
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::reference::copy_configuration;

#[test]
fn disposable_configuration_is_mutable_bounded_and_never_reuses_existing_state(
) -> Result<(), io::Error> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-reference-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/interop/asterisk");
        let destination = directory.join("reference");
        copy_configuration(&source, &destination)?;
        assert_eq!(
            std::fs::read(destination.join("ari.conf"))?,
            std::fs::read(source.join("ari.conf"))?
        );
        assert!(!source.join("websocket_client.conf").exists());
        std::fs::write(
            destination.join("websocket_client.conf"),
            "[agent]\ntype=websocket_client\n",
        )?;
        assert!(!source.join("websocket_client.conf").exists());
        assert!(copy_configuration(&source, &destination).is_err());
        assert!(destination.join("websocket_client.conf").is_file());
        let oversized = directory.join("oversized");
        std::fs::create_dir(&oversized)?;
        std::fs::write(oversized.join("oversized.conf"), [b'x'; 65537])?;
        let refused = directory.join("refused");
        assert!(copy_configuration(&oversized, &refused).is_err());
        assert!(!refused.exists());
        Ok::<(), io::Error>(())
    })();
    let cleanup = std::fs::remove_dir_all(directory);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; cleanup failed: {cleanup}"
        ))),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
    }
}
