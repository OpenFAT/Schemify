//! The socket boundary of the command stream: an address → a listening local socket → one
//! byte stream per client (`docs/spec/rpc.md` §2; D7: nothing off this machine can drive the
//! editor). A Unix-domain socket path on Unix, loopback TCP everywhere.
//!
//! Contents:
//! - `pub enum Listener { Tcp, Unix }`, `Listener::accept`
//! - `pub enum Stream { Tcp, Unix }`, `Stream::try_clone`, `Stream::set_write_timeout`,
//!   `impl Read for Stream`, `impl Write for Stream`
//! - `pub fn listen(&str) -> io::Result<Listener>`

use std::io::{self, Read, Write};
use std::path::Path;

/// A listening local socket: a Unix-domain socket path (Unix only) or a loopback TCP
/// `host:port` (every platform).
#[derive(Debug)]
pub enum Listener {
    /// `127.0.0.1:<port>` or `[::1]:<port>`.
    Tcp(std::net::TcpListener),
    /// A socket file.
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
}

/// One client connection of a [`Listener`].
#[derive(Debug)]
pub enum Stream {
    /// TCP.
    Tcp(std::net::TcpStream),
    /// Unix-domain socket.
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

/// Listens on `addr`: a socket address (`127.0.0.1:7777`) is TCP and must be loopback, so
/// nothing off this machine can drive the editor (D7); anything else is a Unix-domain socket
/// path. A stale socket file nobody listens on is replaced; any other existing file is left
/// alone (the bind fails).
///
/// # Errors
///
/// A non-loopback address, a path on a platform without Unix sockets, or the OS refusing
/// the bind.
pub fn listen(addr: &str) -> io::Result<Listener> {
    let Ok(sock) = addr.parse::<std::net::SocketAddr>() else {
        return listen_path(Path::new(addr));
    };
    if !sock.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "only loopback addresses (127.0.0.1, ::1) are served",
        ));
    }
    std::net::TcpListener::bind(sock).map(Listener::Tcp)
}

impl Listener {
    /// Waits for the next client.
    ///
    /// # Errors
    ///
    /// The OS's accept error.
    pub fn accept(&self) -> io::Result<Stream> {
        match self {
            Listener::Tcp(l) => l.accept().map(|(s, _)| Stream::Tcp(s)),
            #[cfg(unix)]
            Listener::Unix(l) => l.accept().map(|(s, _)| Stream::Unix(s)),
        }
    }
}

impl Stream {
    /// A second handle on the same connection (one thread reads, another writes).
    ///
    /// # Errors
    ///
    /// The OS refusing to duplicate the handle.
    pub fn try_clone(&self) -> io::Result<Stream> {
        match self {
            Stream::Tcp(s) => s.try_clone().map(Stream::Tcp),
            #[cfg(unix)]
            Stream::Unix(s) => s.try_clone().map(Stream::Unix),
        }
    }

    /// Bounds how long a write may block on a client that stops reading.
    ///
    /// # Errors
    ///
    /// The OS refusing the option.
    pub fn set_write_timeout(&self, t: Option<std::time::Duration>) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_write_timeout(t),
            #[cfg(unix)]
            Stream::Unix(s) => s.set_write_timeout(t),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Stream::Unix(s) => s.flush(),
        }
    }
}

// ---- private ----

/// Binds a Unix-domain socket at `path`, replacing a stale socket file first.
#[cfg(unix)]
fn listen_path(path: &Path) -> io::Result<Listener> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    let stale = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
        && UnixStream::connect(path).is_err();
    if stale {
        std::fs::remove_file(path)?;
    }
    UnixListener::bind(path).map(Listener::Unix)
}

#[cfg(not(unix))]
fn listen_path(_: &Path) -> io::Result<Listener> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unix-domain sockets need Unix: listen on 127.0.0.1:<port>",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Negative space of D7: a non-loopback address is refused before any bind.
    #[test]
    fn only_loopback_addresses_are_served() {
        let e = listen("0.0.0.0:0").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(matches!(listen("127.0.0.1:0"), Ok(Listener::Tcp(_))));
    }
}
