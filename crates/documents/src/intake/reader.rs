use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};

use tokio::io::{AsyncRead, ReadBuf};

use super::{HEAD_BYTES, Rejection, ZipLimits, check_head, scan_zip};
use crate::Format;

/// Passes bytes through while checking them. A refusal is returned as a read error carrying the
/// [`Rejection`] (see [`ValidatingReader::rejection`]); at EOF that comes before the end is
/// reported, so storage never keeps a refused file.
pub struct ValidatingReader<R> {
    inner: R,
    format: Format,
    limits: ZipLimits,
    head: Vec<u8>,
    head_checked: bool,
    tail: Vec<u8>,
    tail_cap: usize,
    total: u64,
    finished: bool,
}

impl<R> ValidatingReader<R> {
    pub fn new(inner: R, format: Format, limits: ZipLimits) -> Self {
        Self {
            inner,
            format,
            limits,
            head: Vec::with_capacity(HEAD_BYTES),
            head_checked: false,
            tail: Vec::new(),
            tail_cap: limits.tail_bytes(),
            total: 0,
            finished: false,
        }
    }

    /// The refusal carried by an error this reader returned.
    pub fn rejection(err: &io::Error) -> Option<Rejection> {
        err.get_ref()?.downcast_ref::<Rejection>().copied()
    }

    fn observe(&mut self, data: &[u8]) -> Result<(), Rejection> {
        self.total += data.len() as u64;
        if !self.head_checked {
            let take = (HEAD_BYTES - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..take]);
            if self.head.len() == HEAD_BYTES {
                self.head_checked = true;
                check_head(self.format, &self.head)?;
            }
        }
        if self.format.is_ooxml() {
            self.tail.extend_from_slice(data);
            // Trimmed in batches, so each byte is copied a bounded number of times.
            if self.tail.len() > self.tail_cap.saturating_mul(2) {
                self.tail.drain(..self.tail.len() - self.tail_cap);
            }
        }
        Ok(())
    }

    fn finish(&self) -> Result<(), Rejection> {
        if !self.head_checked {
            check_head(self.format, &self.head)?;
        }
        if self.format.is_ooxml() {
            let tail = &self.tail[self.tail.len().saturating_sub(self.tail_cap)..];
            scan_zip(tail, self.total, &self.limits)?;
        }
        Ok(())
    }
}

fn refused(rejection: Rejection) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, rejection)
}

impl<R: AsyncRead + Unpin> AsyncRead for ValidatingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        let read = &buf.filled()[before..];
        if read.is_empty() {
            this.finished = true;
            return Poll::Ready(this.finish().map_err(refused));
        }
        if let Err(rejection) = this.observe(read) {
            // A failing read must report no bytes (AsyncRead contract, asserted by tokio).
            buf.set_filled(before);
            return Poll::Ready(Err(refused(rejection)));
        }
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::intake::testing::{ZipEntry, zip};

    async fn read(format: Format, bytes: Vec<u8>) -> Result<Vec<u8>, Option<Rejection>> {
        let mut reader =
            ValidatingReader::new(std::io::Cursor::new(bytes), format, ZipLimits::DEFAULT);
        let mut out = Vec::new();
        match reader.read_to_end(&mut out).await {
            Ok(_) => Ok(out),
            Err(e) => Err(ValidatingReader::<()>::rejection(&e)),
        }
    }

    #[tokio::test]
    async fn valid_files_pass_through_unchanged() {
        let docx = zip(&[ZipEntry::stored("[Content_Types].xml", &[b'x'; 5000])]);
        assert_eq!(read(Format::Docx, docx.clone()).await, Ok(docx));
        let pdf = b"%PDF-1.4\n%%EOF\n".to_vec();
        assert_eq!(read(Format::Pdf, pdf.clone()).await, Ok(pdf));
    }

    #[tokio::test]
    async fn refusals_surface_as_read_errors() {
        assert_eq!(
            read(Format::Pdf, b"PK\x03\x04".to_vec()).await,
            Err(Some(Rejection::InvalidSignature))
        );
        assert_eq!(
            read(Format::Docx, Vec::new()).await,
            Err(Some(Rejection::InvalidSignature))
        );
        let bomb = zip(&[ZipEntry::claiming("x", b"tiny", 3 << 30)]);
        assert_eq!(read(Format::Xlsx, bomb).await, Err(Some(Rejection::Bomb)));
        let mut late = vec![0u8; 2048];
        late[..4].copy_from_slice(b"%PDF");
        assert_eq!(
            read(Format::Pdf, late).await,
            Err(Some(Rejection::InvalidSignature))
        );
    }
}
