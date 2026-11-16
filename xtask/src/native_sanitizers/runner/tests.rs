use super::reserve_directory;
use std::io;

#[test]
fn native_directory_reservations_preserve_collisions_and_refuse_exhaustion() -> io::Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-native-reservation-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let operation = (|| {
        let foreign = root.join("rusterisk-native-sanitizers-fixture");
        std::fs::create_dir(&foreign)?;
        std::fs::write(foreign.join("foreign"), b"keep")?;
        let (identity, directory) = reserve_directory(&root, "fixture")?;
        assert_eq!(identity, "fixture-1");
        assert_ne!(directory, foreign);
        assert_eq!(std::fs::read(foreign.join("foreign"))?, b"keep");
        for attempt in 2..32 {
            std::fs::create_dir(
                root.join(format!("rusterisk-native-sanitizers-fixture-{attempt}")),
            )?;
        }
        assert!(
            matches!(reserve_directory(&root,"fixture"),Err(error) if error.kind()==io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            std::fs::read_dir(&root)?
                .collect::<io::Result<Vec<_>>>()?
                .len(),
            32
        );
        assert_eq!(std::fs::read(foreign.join("foreign"))?, b"keep");
        Ok(())
    })();
    match (operation, std::fs::remove_dir_all(root)) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; native reservation fixture cleanup failed: {cleanup}"
        ))),
    }
}
