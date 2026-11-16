use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

struct File {
    path: &'static str,
    bytes: Vec<u8>,
}

pub struct ServerRuntimeConfig {
    files: [File; 5],
}

fn sample(
    path: &'static str,
    read: &mut impl FnMut(&str) -> io::Result<Vec<u8>>,
) -> io::Result<File> {
    let bytes = read(path)?;
    if bytes.is_empty()
        || bytes.len() > 65_536
        || std::str::from_utf8(&bytes).is_err()
        || bytes
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\r' | b'\n' | b'\t'))
    {
        return Err(io::Error::other(
            "runtime configuration requires bounded sample text",
        ));
    }
    Ok(File { path, bytes })
}

fn replace(file: &mut File, expected: &str, replacement: &str) -> io::Result<()> {
    let source = std::str::from_utf8(&file.bytes).map_err(io::Error::other)?;
    if source.matches(expected).count() != 1 {
        return Err(io::Error::other("server listener sample profile changed"));
    }
    file.bytes = source.replace(expected, replacement).into_bytes();
    Ok(())
}

impl ServerRuntimeConfig {
    pub fn capture(
        http: SocketAddr,
        sip: SocketAddr,
        mut read: impl FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> io::Result<Self> {
        if [http, sip]
            .iter()
            .any(|address| address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0)
        {
            return Err(io::Error::other(
                "runtime samples require bound IPv4 loopback ports",
            ));
        }
        let mut http_file = sample("config/http.conf", &mut read)?;
        let mut sip_file = sample("config/pjsip.conf", &mut read)?;
        replace(
            &mut http_file,
            "bindport=8088\n",
            &format!("bindport={}\n", http.port()),
        )?;
        replace(
            &mut sip_file,
            "bind=127.0.0.1:5060\n",
            &format!("bind={sip}\n"),
        )?;
        Ok(Self {
            files: [
                http_file,
                sample("config/ari.conf", &mut read)?,
                sip_file,
                sample("config/rtp.conf", &mut read)?,
                sample("config/extensions.conf", &mut read)?,
            ],
        })
    }

    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files
            .iter()
            .map(|file| (file.path, file.bytes.as_slice()))
    }
}
