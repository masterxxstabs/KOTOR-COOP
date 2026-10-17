use crate::hub::{Flow, OUTBOX_CAPACITY};
use crate::SharedHub;
use kotor_protocol::*;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Clients ping and stream state constantly; silence this long means they are gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// Generous: 20 Hz state + chat + events, with headroom for bursts.
const MAX_MSGS_PER_SEC: u32 = 200;

pub async fn accept_loop(listener: TcpListener, hub: SharedHub, mut stop: watch::Receiver<bool>) {
    loop {
        let accepted = tokio::select! {
            r = listener.accept() => r,
            _ = stop.changed() => return,
        };
        match accepted {
            Ok((stream, addr)) => {
                let hub = hub.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, addr, hub).await {
                        tracing::debug!("{addr}: {e:#}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn read_frame<T: serde::de::DeserializeOwned>(
    rd: &mut OwnedReadHalf,
    buf: &mut Vec<u8>,
) -> anyhow::Result<T> {
    let mut len = [0u8; 4];
    rd.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME_LEN, "frame too large ({len} bytes)");
    buf.resize(len, 0);
    rd.read_exact(buf).await?;
    Ok(decode_payload(buf)?)
}

async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    hub: SharedHub,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut rd, mut wr) = stream.into_split();
    let mut buf = Vec::new();

    let hello =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut rd, &mut buf)).await?? {
            ClientMessage::Hello(h) => h,
            _ => anyhow::bail!("expected Hello"),
        };

    let (tx, mut rx) = mpsc::channel::<Arc<Vec<u8>>>(OUTBOX_CAPACITY);
    let joined = hub.lock().unwrap().join(&hello, tx);
    let joined = match joined {
        Ok(j) => j,
        Err((reason, message)) => {
            tracing::info!("{addr} rejected ({reason:?}): {message}");
            let frame = encode_frame(&ServerMessage::Rejected { reason, message })?;
            wr.write_all(&frame).await?;
            wr.shutdown().await?;
            return Ok(());
        }
    };
    let id = joined.id;

    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wr.write_all(&frame).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });

    let mut window_start = tokio::time::Instant::now();
    let mut window_count = 0u32;
    let result: anyhow::Result<()> = async {
        loop {
            let msg = tokio::select! {
                r = tokio::time::timeout(IDLE_TIMEOUT, read_frame::<ClientMessage>(&mut rd, &mut buf)) => r??,
                _ = joined.kick.notified() => return Ok(()),
            };
            let now = tokio::time::Instant::now();
            if now - window_start >= Duration::from_secs(1) {
                window_start = now;
                window_count = 0;
            }
            window_count += 1;
            anyhow::ensure!(window_count <= MAX_MSGS_PER_SEC, "message flood");
            if hub.lock().unwrap().handle(id, msg) == Flow::Disconnect {
                return Ok(());
            }
        }
    }
    .await;

    hub.lock().unwrap().leave(id);
    // Leaving dropped the hub's sender; the writer drains what's queued and exits.
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    result
}
