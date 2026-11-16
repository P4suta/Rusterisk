use std::{io, path::Path};
use xtask::release_check::{InputError, SourceInputs};

#[test]
fn runner_inputs_bind_to_the_same_bytes_without_using_an_unrelated_snapshot_digest(
) -> io::Result<()> {
    let inputs = [
        (Path::new("Cargo.toml"), b"workspace".as_slice()),
        (Path::new("crates/core/src/lib.rs"), b"core".as_slice()),
        (
            Path::new("docs/server-distribution.md"),
            b"server".as_slice(),
        ),
    ];
    let cohort = SourceInputs::new(inputs.into_iter()).map_err(io::Error::other)?;
    let sdk = SourceInputs::new(inputs[..2].iter().copied()).map_err(io::Error::other)?;
    let server = SourceInputs::new(inputs.into_iter().rev()).map_err(io::Error::other)?;
    assert_eq!(cohort.bind(&sdk).map_err(io::Error::other)?, sdk.key());
    assert_eq!(
        cohort.bind(&server).map_err(io::Error::other)?,
        cohort.key()
    );
    assert_ne!(sdk.key(), server.key());
    let changed = SourceInputs::new(
        [(Path::new("crates/core/src/lib.rs"), b"changed".as_slice())].into_iter(),
    )
    .map_err(io::Error::other)?;
    assert!(matches!(cohort.bind(&changed), Err(InputError::Changed)));
    let missing = SourceInputs::new(
        [(
            Path::new("crates/core/proofs/Cargo.lock"),
            b"locked".as_slice(),
        )]
        .into_iter(),
    )
    .map_err(io::Error::other)?;
    assert!(matches!(cohort.bind(&missing), Err(InputError::Missing)));
    Ok(())
}

#[test]
fn empty_duplicate_and_escaping_input_paths_are_rejected() {
    assert!(matches!(
        SourceInputs::new(std::iter::empty()),
        Err(InputError::Empty)
    ));
    let same = (Path::new("Cargo.toml"), b"workspace".as_slice());
    assert!(matches!(
        SourceInputs::new([same, same].into_iter()),
        Err(InputError::Duplicate)
    ));
    for path in [
        "../escape",
        "/absolute",
        "source/../escape",
        "source/./escape",
    ] {
        assert!(matches!(
            SourceInputs::new([(Path::new(path), b"data".as_slice())].into_iter()),
            Err(InputError::Path)
        ));
    }
    if !cfg!(windows) {
        assert!(matches!(
            SourceInputs::new([(Path::new("source\\escape"), b"data".as_slice())].into_iter()),
            Err(InputError::Path)
        ));
    }
}

#[test]
fn source_collections_stop_at_the_first_excess_file_and_bound_the_total_bytes() -> io::Result<()> {
    let names: Vec<_> = (0..=SourceInputs::MAX_FILES)
        .map(|index| format!("src/file{index}.rs"))
        .collect();
    let full = SourceInputs::new(
        names[..SourceInputs::MAX_FILES]
            .iter()
            .map(|name| (Path::new(name), b"x".as_slice())),
    )
    .map_err(io::Error::other)?;
    assert_eq!(full.len(), SourceInputs::MAX_FILES);
    let mut seen = 0;
    let excess = SourceInputs::new(
        names
            .iter()
            .map(|name| (Path::new(name), b"x".as_slice()))
            .inspect(|_| seen += 1),
    );
    assert!(matches!(excess, Err(InputError::Limit)));
    assert_eq!(seen, SourceInputs::MAX_FILES + 1);
    let block = vec![0; 1024 * 1024];
    let large = SourceInputs::new(
        names[..40]
            .iter()
            .map(|name| (Path::new(name), block.as_slice())),
    )
    .map_err(io::Error::other)?;
    assert_eq!(large.len(), 40);
    assert!(matches!(
        SourceInputs::new(
            names[..41]
                .iter()
                .map(|name| (Path::new(name), block.as_slice()))
        ),
        Err(InputError::Limit)
    ));
    Ok(())
}
