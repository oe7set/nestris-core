//! Reader/writer adapters over `std::io` for NGF streams, plus gzip
//! handling. The codec itself ([`crate::codec`]) stays sans-io.
//!
//! Gzip is detected by content (the `1f 8b` magic), not by file extension,
//! so renamed files and in-memory buffers behave identically.

use std::io::{self, Read, Write};

use crate::codec::{self, NgfFrame};

/// Gzip magic bytes.
const GZ_MAGIC: [u8; 2] = [0x1F, 0x8B];

/// Streaming v3 writer: appends one encoded frame per [`NgfWriter::write`]
/// call. Used for the crash-safe `.ngf.part` recording path, where every
/// frame must hit the underlying writer promptly.
pub struct NgfWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
    frames: usize,
}

impl<W: Write> NgfWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(codec::V3_FRAME_SIZE),
            frames: 0,
        }
    }

    pub fn write(&mut self, frame: &NgfFrame) -> io::Result<()> {
        self.buf.clear();
        codec::encode_v3(frame, &mut self.buf);
        self.inner.write_all(&self.buf)?;
        self.frames += 1;
        Ok(())
    }

    pub fn frames_written(&self) -> usize {
        self.frames
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

/// Read a whole NGF stream (plain or gzipped) into decoded frames.
///
/// NGF files are small (73 bytes/frame ≈ a few MB for a long game), so the
/// whole-buffer approach keeps the API simple for both the replay engine
/// and wasm (which receives the file as one byte buffer anyway).
pub fn read_all<R: Read>(mut reader: R) -> io::Result<Vec<NgfFrame>> {
    let mut raw = Vec::new();
    reader.read_to_end(&mut raw)?;
    decode_all(&raw)
}

/// Decode a complete in-memory NGF byte buffer (plain or gzipped).
pub fn decode_all(bytes: &[u8]) -> io::Result<Vec<NgfFrame>> {
    let plain: Vec<u8>;
    let mut data: &[u8] = bytes;
    if bytes.starts_with(&GZ_MAGIC) {
        #[cfg(feature = "gz")]
        {
            let mut decoder = flate2::read::GzDecoder::new(bytes);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            plain = out;
            data = &plain;
        }
        #[cfg(not(feature = "gz"))]
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gzipped NGF but the `gz` feature is disabled",
            ));
        }
    }

    let mut frames = Vec::with_capacity(data.len() / codec::V3_FRAME_SIZE);
    let mut offset = 0;
    while offset < data.len() {
        let (frame, consumed) = codec::decode_frame(&data[offset..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        frames.push(frame);
        offset += consumed;
    }
    Ok(frames)
}

/// Gzip-compress an encoded NGF buffer (for `.ngf.gz` finalization and the
/// browser download path).
#[cfg(feature = "gz")]
pub fn compress_gz(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<NgfFrame> {
        (0..5)
            .map(|i| NgfFrame {
                gameid: 9,
                ctime_ms: i * 16,
                score: Some(i * 100),
                ..NgfFrame::default()
            })
            .collect()
    }

    #[test]
    fn writer_reader_round_trip() {
        let mut w = NgfWriter::new(Vec::new());
        for f in frames() {
            w.write(&f).unwrap();
        }
        assert_eq!(w.frames_written(), 5);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), 5 * codec::V3_FRAME_SIZE);
        let decoded = decode_all(&bytes).unwrap();
        assert_eq!(decoded, frames());
    }

    #[cfg(feature = "gz")]
    #[test]
    fn gzip_round_trip_by_content_sniffing() {
        let mut w = NgfWriter::new(Vec::new());
        for f in frames() {
            w.write(&f).unwrap();
        }
        let gz = compress_gz(&w.into_inner()).unwrap();
        assert!(gz.starts_with(&[0x1F, 0x8B]));
        let decoded = decode_all(&gz).unwrap();
        assert_eq!(decoded, frames());
    }

    #[test]
    fn trailing_garbage_is_an_error() {
        let mut w = NgfWriter::new(Vec::new());
        w.write(&NgfFrame::default()).unwrap();
        let mut bytes = w.into_inner();
        bytes.extend_from_slice(&[0xAA; 10]);
        assert!(decode_all(&bytes).is_err());
    }
}
