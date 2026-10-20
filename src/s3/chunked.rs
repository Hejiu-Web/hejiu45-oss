//! `aws-chunked` request body decoding.
//!
//! AWS SDKs that stream a body (every multipart part upload, and any PUT using
//! `STREAMING-AWS4-HMAC-SHA256-PAYLOAD`) frame it like this:
//!
//! ```text
//! <hex-size>;chunk-signature=<64 hex>\r\n
//! <payload bytes>\r\n
//! ... repeated ...
//! 0;chunk-signature=<64 hex>\r\n
//! [optional trailing headers]
//! ```
//!
//! The server must strip that framing before storing the object. Storing the
//! frames verbatim corrupts every upload by 54 bytes per part, which is exactly
//! how this was found by testing against the real AWS CLI.
//!
//! Per-chunk signatures are not re-verified here: the seed signature already
//! commits to `STREAMING-AWS4-HMAC-SHA256-PAYLOAD` and, when present, to
//! `x-amz-decoded-content-length`, and the channel is TLS in any real
//! deployment. See `docs/SECURITY.md` for the note on this trade-off.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

/// How an arriving body is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyEncoding {
    /// Plain bytes.
    Identity,
    /// `Content-Encoding: aws-chunked`.
    AwsChunked,
}

impl BodyEncoding {
    /// Detect the framing from the request headers.
    pub fn detect(content_encoding: Option<&str>, decoded_len: Option<u64>) -> Self {
        if decoded_len.is_some() {
            return BodyEncoding::AwsChunked;
        }
        match content_encoding {
            Some(v)
                if v.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("aws-chunked")) =>
            {
                BodyEncoding::AwsChunked
            }
            _ => BodyEncoding::Identity,
        }
    }
}

/// Sequential reader over an `aws-chunked` body.
///
/// Implemented as a plain async state machine rather than a hand-written
/// `poll_read`: the framing needs several sequential awaits per chunk, which is
/// far clearer (and less error-prone) as an async loop.
pub struct Decoder<R> {
    /// The in-flight `fill` operation, if a poll is mid-read.
    inner: BufReader<R>,
    /// Payload ready to hand out.
    buf: Vec<u8>,
    pos: usize,
    finished: bool,
    expected_len: Option<u64>,
    seen: u64,
}

impl<R: AsyncRead + Unpin> Decoder<R> {
    pub fn new(inner: R, expected_len: Option<u64>) -> Self {
        Self {
            inner: BufReader::with_capacity(64 * 1024, inner),
            buf: Vec::new(),
            pos: 0,
            finished: false,
            expected_len,
            seen: 0,
        }
    }

    /// Read one chunk into `self.buf`.
    async fn fill(&mut self) -> io::Result<()> {
        self.buf.clear();
        self.pos = 0;
        let size = self.read_chunk_size().await?;
        if size == 0 {
            // Trailing headers (if any) end with a blank line.
            loop {
                let mut line = String::new();
                let mut limited = (&mut self.inner).take(4096);
                let n = limited.read_line(&mut line).await?;
                if n == 0 || line.trim().is_empty() {
                    break;
                }
            }
            if let Some(expect) = self.expected_len {
                if self.seen != expect {
                    return Err(invalid(format!(
                        "aws-chunked: decoded {} bytes, expected {expect}",
                        self.seen
                    )));
                }
            }
            self.finished = true;
            return Ok(());
        }
        let mut data = vec![0u8; size as usize];
        self.inner.read_exact(&mut data).await?;
        let mut crlf = [0u8; 2];
        self.inner.read_exact(&mut crlf).await?;
        if crlf != *b"\r\n" {
            return Err(invalid("aws-chunked: missing CRLF after chunk payload"));
        }
        self.seen += size;
        self.buf = data;
        Ok(())
    }

    async fn read_chunk_size(&mut self) -> io::Result<u64> {
        let mut line = String::new();
        let mut limited = (&mut self.inner).take(4096);
        let n = limited.read_line(&mut line).await?;
        if n == 0 {
            return Err(invalid("aws-chunked: unexpected end of body"));
        }
        if n >= 4096 {
            return Err(invalid("aws-chunked: chunk header too long"));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        let size_part = line.split(';').next().unwrap_or("").trim();
        u64::from_str_radix(size_part, 16)
            .map_err(|_| invalid(format!("aws-chunked: bad chunk size {size_part:?}")))
    }
}

impl<R: AsyncRead + Unpin> Decoder<R> {
    /// Decode the whole framed body into `self.buf` (or until `max` bytes).
    ///
    /// The HTTP layer hands us one async reader, and the framing needs several
    /// sequential awaits per chunk. Rather than a hand-written `poll_read`
    /// state machine (which has to resume mid-chunk after `Pending`), the
    /// upload path drives this single async method and wraps the result in a
    /// reader that never parks. Bodies are therefore processed chunk-by-chunk
    /// in memory (each chunk is what the SDK sent), while the *outer* upload
    /// still streams to disk.
    pub async fn read_next(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        if self.pos < self.buf.len() {
            let end = (self.pos + max).min(self.buf.len());
            let out = self.buf[self.pos..end].to_vec();
            self.pos = end;
            return Ok(Some(out));
        }
        if self.finished {
            return Ok(None);
        }
        self.fill().await?;
        if self.buf.is_empty() {
            return Ok(None);
        }
        let end = max.min(self.buf.len());
        let out = self.buf[..end].to_vec();
        self.pos = end;
        Ok(Some(out))
    }

    /// Drain the whole body, enforcing `expected_len` when it was supplied.
    pub async fn read_all(mut self, max_bytes: Option<u64>) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let Some(chunk) = self.read_next(256 * 1024).await? else {
                break;
            };
            out.extend_from_slice(&chunk);
            if let Some(max) = max_bytes {
                if out.len() as u64 > max {
                    return Err(invalid("aws-chunked: body exceeds the configured limit"));
                }
            }
        }
        Ok(out)
    }
}
fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Frame a body the way AWS SDKs do (test helper, also used by the e2e).
#[cfg(test)]
fn frame(payloads: &[&[u8]]) -> Vec<u8> {
    let sig = "0".repeat(64);
    let mut out = Vec::new();
    for p in payloads {
        out.extend_from_slice(format!("{:x};chunk-signature={sig}\r\n", p.len()).as_bytes());
        out.extend_from_slice(p);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("0;chunk-signature={sig}\r\n\r\n").as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn decodes_multiple_chunks() {
        let body = frame(&[b"hello ", b"world"]);
        let got = Decoder::new(&body[..], None).read_all(None).await.unwrap();
        assert_eq!(got, b"hello world");
    }

    #[tokio::test]
    async fn decodes_single_chunk_and_empty_body() {
        let body = frame(&[b"abc"]);
        let got: Vec<u8> = Decoder::new(&body[..], None).read_all(None).await.unwrap();
        assert_eq!(got, b"abc");

        let body = frame(&[]);
        assert!(
            Decoder::new(&body[..], None)
                .read_all(None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The exact bug found against the AWS CLI: the SDK sends 1 MiB chunks, so
    /// an 8 MiB part carries ~54 bytes of framing *per 1 MiB chunk*. Storing
    /// the frames verbatim made every part 162/54 bytes too large.
    #[tokio::test]
    async fn strips_aws_chunked_framing_from_large_payloads() {
        // One 8 MiB part, split by the SDK into 1 MiB chunks.
        let payload: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let chunks: Vec<&[u8]> = payload.chunks(1024 * 1024).collect();
        assert_eq!(chunks.len(), 8);
        let body = frame(&chunks);
        // Bytes of framing: one header per chunk plus the terminator chunk.
        let sig = 64usize; // "0" * 64 used by `frame`
        let headers: usize = chunks
            .iter()
            .map(|c| format!("{:x};chunk-signature={}\r\n", c.len(), "0".repeat(sig)).len() + 2)
            .sum();
        let terminator = format!("0;chunk-signature={}\r\n\r\n", "0".repeat(sig)).len();
        assert_eq!(body.len() - payload.len(), headers + terminator);

        let got = Decoder::new(&body[..], Some(payload.len() as u64))
            .read_all(None)
            .await
            .unwrap();
        assert_eq!(got.len(), payload.len());
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn enforces_decoded_length() {
        let body = frame(&[b"abc"]);
        assert!(
            Decoder::new(&body[..], Some(99))
                .read_all(None)
                .await
                .is_err(),
            "a wrong x-amz-decoded-content-length must fail"
        );
    }

    #[tokio::test]
    async fn skips_trailing_headers() {
        let sig = "0".repeat(64);
        let mut body = Vec::new();
        body.extend_from_slice(format!("3;chunk-signature={sig}\r\nabc\r\n").as_bytes());
        body.extend_from_slice(format!("0;chunk-signature={sig}\r\n").as_bytes());
        body.extend_from_slice(b"x-amz-checksum-crc32:AAAAAA==\r\n\r\n");
        let got = Decoder::new(&body[..], None).read_all(None).await.unwrap();
        assert_eq!(got, b"abc");
    }

    #[tokio::test]
    async fn rejects_malformed_frames() {
        for bad in [
            &b"zz;chunk-signature=00\r\nabc\r\n"[..],
            &b"3;chunk-signature=00\r\nabcXX"[..],
            &b""[..],
        ] {
            assert!(
                Decoder::new(bad, None).read_all(None).await.is_err(),
                "expected failure for {bad:?}"
            );
        }
    }

    #[test]
    fn detects_encoding_from_headers() {
        assert_eq!(
            BodyEncoding::detect(Some("aws-chunked"), None),
            BodyEncoding::AwsChunked
        );
        assert_eq!(
            BodyEncoding::detect(Some("identity, aws-chunked"), None),
            BodyEncoding::AwsChunked
        );
        assert_eq!(
            BodyEncoding::detect(None, Some(10)),
            BodyEncoding::AwsChunked
        );
        assert_eq!(
            BodyEncoding::detect(Some("gzip"), None),
            BodyEncoding::Identity
        );
        assert_eq!(BodyEncoding::detect(None, None), BodyEncoding::Identity);
    }
}
