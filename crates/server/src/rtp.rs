use std::io;
use std::net::{IpAddr, SocketAddr};
use tokio::net::UdpSocket;

pub struct SocketPair {
    data: UdpSocket,
    control: UdpSocket,
}

impl SocketPair {
    pub fn bind(ip: IpAddr, start: u16, end: u16) -> Result<Self, io::Error> {
        let ephemeral = start == 0 && end == 0;
        if !ephemeral && (start == 0 || !start.is_multiple_of(2) || start >= end) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid media port range",
            ));
        }
        let attempts = if ephemeral {
            128
        } else {
            (usize::from(end - start) / 2 + 1).min(256)
        };
        for index in 0..attempts {
            let port = if ephemeral {
                0
            } else {
                start + index as u16 * 2
            };
            let data = match std::net::UdpSocket::bind(SocketAddr::new(ip, port)) {
                Ok(socket) => socket,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            };
            let local = data.local_addr()?;
            if !local.port().is_multiple_of(2) {
                continue;
            }
            let Some(control_port) = local.port().checked_add(1) else {
                continue;
            };
            if !ephemeral && control_port > end {
                continue;
            }
            let control = match std::net::UdpSocket::bind(SocketAddr::new(ip, control_port)) {
                Ok(socket) => socket,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            };
            data.set_nonblocking(true)?;
            control.set_nonblocking(true)?;
            return Ok(Self {
                data: UdpSocket::from_std(data)?,
                control: UdpSocket::from_std(control)?,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "media port capacity exceeded",
        ))
    }

    pub fn local_addr(&self) -> Result<SocketAddr, io::Error> {
        self.data.local_addr()
    }

    pub fn control_addr(&self) -> Result<SocketAddr, io::Error> {
        self.control.local_addr()
    }
}
