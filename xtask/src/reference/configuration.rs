use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

fn copy(source: &Path, destination: &Path) -> Result<(), io::Error> {
    let mut count = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        count += 1;
        if count > 32
            || !entry.file_type()?.is_file()
            || entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "conf")
        {
            return Err(io::Error::other("invalid reference configuration fixture"));
        }
        let mut bytes = Vec::with_capacity(65537);
        fs::File::open(entry.path())?
            .take(65537)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(io::Error::other(
                "reference configuration file exceeds capacity",
            ));
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(entry.file_name()))?
            .write_all(&bytes)?;
    }
    if count == 0 {
        return Err(io::Error::other("reference configuration fixture is empty"));
    }
    Ok(())
}

pub fn copy_configuration(source: &Path, destination: &Path) -> Result<(), io::Error> {
    fs::create_dir(destination)?;
    let result = copy(source, destination);
    if result.is_ok() {
        return result;
    }
    match (result, fs::remove_dir_all(destination)) {
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; configuration cleanup failed: {cleanup}"
        ))),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
