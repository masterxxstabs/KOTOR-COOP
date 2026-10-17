//! Minimal blocking client used by the game DLL and the test bot. No async
//! runtime, so it stays small inside the 32-bit game process.

use crate::*;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("network: {0}")]
    Io(#[from] io::Error),
    #[error("protocol: {0}")]
    Frame(#[from] FrameError),
    #[error("server refused the connection ({reason:?}): {message}")]
    Rejected {
        reason: RejectReason,
        message: String,
    },
    #[error("server sent {0} before Welcome")]
    Unexpected(&'static str),
    #[error("could not resolve {0}")]
    Resolve(String),
}

/// Reading half. `recv` blocks until a full message arrives.
pub struct Receiver {
    stream: TcpStream,
    decoder: FrameDecoder,
    buf: Box<[u8; 16 * 1024]>,
}

/// Writing half; cheap to clone so several threads can send.
#[derive(Clone)]
pub struct Sender {
    stream: std::sync::Arc<std::sync::Mutex<TcpStream>>,
}

impl Receiver {
    pub fn recv(&mut self) -> Result<ServerMessage, ClientError> {
        loop {
            if let Some(msg) = self.decoder.try_next()? {
                return Ok(msg);
            }
            let n = self.stream.read(&mut self.buf[..])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed the connection",
                )
                .into());
            }
            self.decoder.extend(&self.buf[..n]);
        }
    }
}

impl Sender {
    pub fn send(&self, msg: &ClientMessage) -> Result<(), ClientError> {
        let frame = encode_frame(msg)?;
        self.stream.lock().unwrap().write_all(&frame)?;
        Ok(())
    }

    pub fn close(&self) {
        let _ = self.send(&ClientMessage::Goodbye);
        let _ = self
            .stream
            .lock()
            .unwrap()
            .shutdown(std::net::Shutdown::Both);
    }
}

fn resolve(addr: &str) -> Result<SocketAddr, ClientError> {
    let with_port = if addr
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        addr.to_owned()
    } else {
        format!("{addr}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .ok_or_else(|| ClientError::Resolve(addr.to_owned()))
}

/// Connects, performs the handshake and returns the server's welcome.
/// `addr` may omit the port (defaults to [`DEFAULT_PORT`]).
pub fn connect(addr: &str, hello: Hello) -> Result<(Welcome, Sender, Receiver), ClientError> {
    let sock = resolve(addr)?;
    let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(8))?;
    stream.set_nodelay(true)?;
    let mut rx = Receiver {
        stream: stream.try_clone()?,
        decoder: FrameDecoder::new(),
        buf: Box::new([0; 16 * 1024]),
    };
    let tx = Sender {
        stream: std::sync::Arc::new(std::sync::Mutex::new(stream)),
    };

    tx.send(&ClientMessage::Hello(hello))?;
    rx.stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let welcome = match rx.recv()? {
        ServerMessage::Welcome(w) => w,
        ServerMessage::Rejected { reason, message } => {
            return Err(ClientError::Rejected { reason, message })
        }
        _ => return Err(ClientError::Unexpected("another message")),
    };
    rx.stream.set_read_timeout(None)?;
    Ok((welcome, tx, rx))
}

/// Measures a quick server round trip without joining (used by the launcher's
/// server browser). Opens a TCP connection only; the server drops it after
/// the handshake timeout.
pub fn tcp_ping(addr: &str, timeout: Duration) -> Option<Duration> {
    let sock = resolve(addr).ok()?;
    let start = std::time::Instant::now();
    TcpStream::connect_timeout(&sock, timeout).ok()?;
    Some(start.elapsed())
}
