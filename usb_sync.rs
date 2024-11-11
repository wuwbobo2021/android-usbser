// Related issue: <https://github.com/kevinmehall/nusb/issues/4>.

use crate::Error;
use std::io::ErrorKind;
use std::time::Duration;

type ReadQueue = nusb::transfer::Queue<nusb::transfer::RequestBuffer>;
type WriteQueue = nusb::transfer::Queue<Vec<u8>>;
use futures_lite::FutureExt;

/// Synchronous wrapper of a `nusb` IN transfer queue.
pub struct SyncReader {
    queue: ReadQueue,
    buf: Option<Vec<u8>>,
}
impl SyncReader {
    /// Wraps the asynchronous queue.
    pub fn new(queue: ReadQueue) -> Self {
        Self {
            queue,
            buf: Some(Vec::new()),
        }
    }
    /// It is similar to `read()` in the standard `Read` trait, requiring timeout parameter.
    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let buf_async = self.buf.take().unwrap();
        // Safety: `RequestBuffer::reuse()` may reserve larger capacity to reach buf.len()
        let req = nusb::transfer::RequestBuffer::reuse(buf_async, buf.len());

        self.queue.submit(req);
        let fut = self.queue.next_complete();
        let fut_comp = async { Some(fut.await) };
        let fut_cancel = async {
            async_io::Timer::after(timeout).await;
            None
        };
        let comp = {
            let mut maybe_comp = async_io::block_on(fut_comp.or(fut_cancel));
            if maybe_comp.is_none() {
                self.queue.cancel_all(); // the only one
                if self.queue.pending() == 0 {
                    self.buf.replace(Vec::new());
                    return Err(Error::other("Unable to get the transfer result"));
                }
                let comp = async_io::block_on(self.queue.next_complete());
                maybe_comp.replace(comp);
            }
            maybe_comp.unwrap()
        };
        let len_reveived = comp.data.len();

        use nusb::transfer::TransferError;
        let result = match comp.status {
            Ok(()) => {
                buf[..len_reveived].copy_from_slice(&comp.data);
                Ok(len_reveived)
            }
            Err(TransferError::Cancelled) => {
                if len_reveived > 0 {
                    buf[..len_reveived].copy_from_slice(&comp.data);
                    Ok(len_reveived)
                } else {
                    Err(Error::from(ErrorKind::TimedOut))
                }
            }
            Err(TransferError::Disconnected) => Err(Error::from(ErrorKind::NotConnected)),
            Err(TransferError::Stall) => {
                let _ = self.queue.clear_halt();
                Err(Error::other(TransferError::Stall))
            }
            Err(e) => Err(Error::other(e)),
        };
        self.buf.replace(comp.data);
        result
    }
}

/// Synchronous wrapper of a `nusb` OUT transfer queue.
pub struct SyncWriter {
    queue: WriteQueue,
    buf: Option<Vec<u8>>,
}

impl SyncWriter {
    /// Wraps the asynchronous queue.
    pub fn new(queue: WriteQueue) -> Self {
        Self {
            queue,
            buf: Some(Vec::new()),
        }
    }
    /// It is similar to `write()` in the standard `Write` trait, requiring timeout parameter.
    /// It is always synchronous, and `flush()` is not needed.
    pub fn write(&mut self, buf: &[u8], timeout: Duration) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut buf_async = self.buf.take().unwrap();
        buf_async.clear(); // it has no effect on the allocated capacity
        buf_async.extend_from_slice(buf);

        self.queue.submit(buf_async);
        let fut = self.queue.next_complete();
        let fut_comp = async { Some(fut.await) };
        let fut_cancel = async {
            async_io::Timer::after(timeout).await;
            None
        };
        let comp = {
            let mut maybe_comp = async_io::block_on(fut_comp.or(fut_cancel));
            if maybe_comp.is_none() {
                self.queue.cancel_all(); // the only one
                if self.queue.pending() == 0 {
                    self.buf.replace(Vec::new());
                    return Err(Error::other("Unable to get the transfer result"));
                }
                let comp = async_io::block_on(self.queue.next_complete());
                maybe_comp.replace(comp);
            }
            maybe_comp.unwrap()
        };
        let len_sent = comp.data.actual_length();

        use nusb::transfer::TransferError;
        let result = match comp.status {
            Ok(()) => Ok(len_sent),
            Err(TransferError::Cancelled) => {
                if len_sent > 0 {
                    Ok(len_sent)
                } else {
                    Err(Error::from(ErrorKind::TimedOut))
                }
            }
            Err(TransferError::Disconnected) => Err(Error::from(ErrorKind::NotConnected)),
            Err(TransferError::Stall) => {
                let _ = self.queue.clear_halt();
                Err(Error::other(TransferError::Stall))
            }
            Err(e) => Err(Error::other(e)),
        };
        self.buf.replace(comp.data.reuse());
        result
    }
}
