//! Message framing for the PrivCord game tunnel (extracted from PrivCord's `tunnel.rs`).
//!
//! The BOTW Multiplayer dedicated server assumes one `Receive` returns one whole message. That holds on loopback,
//! but a tunnel delivers a TCP stream in arbitrary pieces, and the server then appends its whole 10 KB receive buffer
//! per read, corrupting the stream. PrivCord's tunnel host re-cuts the friend → server stream into whole messages
//! before the server sees it. For BotW: type 1 (ping) is 6144 bytes, everything else 7168.


/// Forward a stream in whole messages: one `write_all` per message, plus whatever is left at EOF. A message's size
/// comes from its first byte (`framing`). Some game servers assume one `recv` returns one message (true on loopback,
/// where the client's single write arrives whole), but the tunnel delivers arbitrary chunks. The BOTW Multiplayer
/// server, for one, appends its whole receive buffer per read, so a message split by the tunnel is corrupted and
/// every later one is misaligned.
pub async fn copy_framed<R, W>(r: &mut R, w: &mut W, framing: &TcpFraming) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let max = framing.by_first_byte.iter().map(|&(_, s)| s).chain([framing.default]).max().unwrap_or(1).max(1);
    let mut buf = vec![0u8; max];
    loop {
        let mut filled = 0;
        let mut size = max;
        while filled < size {
            let n = r.read(&mut buf[filled..size]).await?;
            if n == 0 {
                if filled > 0 {
                    w.write_all(&buf[..filled]).await?;
                }
                return Ok(());
            }
            if filled == 0 {
                size = framing.size_for(buf[0]).clamp(1, max);
                if n > size {
                    // Can't happen for the reads above (bounded by `max`) unless a short message type is followed by
                    // more data in the same read; keep the extra bytes for the next message.
                    w.write_all(&buf[..size]).await?;
                    buf.copy_within(size..n, 0);
                    filled = n - size;
                    size = framing.size_for(buf[0]).clamp(1, max);
                    continue;
                }
            }
            filled += n;
        }
        w.write_all(&buf[..size]).await?;
    }
}

/// How a TCP stream from a friend is cut into messages before it reaches the game server: each message's size is
/// chosen by its first byte (its message type), falling back to `default`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpFraming {
    pub by_first_byte: Vec<(u8, usize)>,
    pub default: usize,
}

impl TcpFraming {
    pub fn size_for(&self, first: u8) -> usize {
        self.by_first_byte.iter().find(|&&(b, _)| b == first).map_or(self.default, |&(_, s)| s)
    }
}

/// The framing PrivCord applies to the BOTW Multiplayer server port (TCP 5050).
pub fn botw_multiplayer() -> (u16, TcpFraming) {
    (5050, TcpFraming { by_first_byte: vec![(1, 6144)], default: 7168 })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct WriteSizes(Vec<usize>, Vec<u8>);
    impl tokio::io::AsyncWrite for WriteSizes {
        fn poll_write(mut self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>, b: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
            self.0.push(b.len());
            self.1.extend_from_slice(b);
            std::task::Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Two 7168-byte messages arriving in uneven pieces (as the tunnel delivers them) reach the server as exactly two
    /// whole writes, byte for byte; a trailing partial message is flushed at EOF.
    #[tokio::test]
    async fn framed_copy_delivers_whole_messages() {
        use tokio::io::AsyncWriteExt;
        let data: Vec<u8> = (0..7168 * 2 + 100).map(|i| (i % 251) as u8).collect();
        let (mut tx, mut rx) = tokio::io::duplex(1500);
        let feed = data.clone();
        let writer = tokio::spawn(async move {
            for chunk in feed.chunks(1371) {
                tx.write_all(chunk).await.unwrap();
            }
        });
        let mut out = WriteSizes(Vec::new(), Vec::new());
        copy_framed(&mut rx, &mut out, &TcpFraming { by_first_byte: vec![], default: 7168 }).await.unwrap();
        writer.await.unwrap();
        assert_eq!(out.0, vec![7168, 7168, 100]);
        assert_eq!(out.1, data);
    }

    /// The message size follows the first byte: a 6144-byte ping (type 1) is forwarded as soon as it is complete
    /// (the client waits for the reply), and a 7168-byte update after it is still delivered whole.
    #[tokio::test]
    async fn framed_copy_sizes_by_first_byte() {
        use tokio::io::AsyncWriteExt;
        let mut ping = vec![0u8; 6144];
        ping[0] = 1;
        let mut update = vec![7u8; 7168];
        update[0] = 3;
        let (mut tx, mut rx) = tokio::io::duplex(1500);
        let framing = TcpFraming { by_first_byte: vec![(1, 6144)], default: 7168 };
        let reader = tokio::spawn(async move {
            let mut out = WriteSizes(Vec::new(), Vec::new());
            copy_framed(&mut rx, &mut out, &framing).await.unwrap();
            out
        });
        for chunk in ping.chunks(1000).chain(update.chunks(999)) {
            tx.write_all(chunk).await.unwrap();
        }
        drop(tx);
        let out = reader.await.unwrap();
        assert_eq!(out.0, vec![6144, 7168]);
        assert_eq!(&out.1[..6144], &ping[..]);
        assert_eq!(&out.1[6144..], &update[..]);
    }
}
