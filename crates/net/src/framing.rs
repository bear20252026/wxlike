//! Length-prefixed framing over QUIC streams: `u32 BE len || bytes`.

use std::io;

pub const MAX_FRAME: u32 = 8 * 1024 * 1024;

pub async fn write_frame<W: tokio::io::AsyncWriteExt + Unpin>(
    w: &mut W,
    payload: &[u8],
) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "frame exceeds u32 length")
    })?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await
}

pub async fn read_frame<R: tokio::io::AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame len {len} exceeds cap"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let payload = b"hello frame".to_vec();
        let (w, r) = tokio::join!(
            async { write_frame(&mut client, &payload).await },
            async { read_frame(&mut server).await }
        );
        w.unwrap();
        assert_eq!(r.unwrap(), payload);
    }
}
