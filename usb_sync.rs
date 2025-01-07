// Related issue: <https://github.com/kevinmehall/nusb/issues/4>.

use crate::Error;
use jni_min_helper::block_for_timeout;

use futures_lite::future::block_on;
use std::{io::ErrorKind, time::Duration};

use nusb::transfer::{Queue, RequestBuffer, TransferError};
type ReadQueue = Queue<RequestBuffer>;
type WriteQueue = Queue<Vec<u8>>;

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
        let comp = {
            let mut maybe_comp = block_for_timeout(fut, timeout);
            if maybe_comp.is_none() {
                self.queue.cancel_all(); // the only one
                if self.queue.pending() == 0 {
                    self.buf.replace(Vec::new());
                    return Err(Error::other("Unable to get the transfer result"));
                }
                let comp = block_on(self.queue.next_complete());
                maybe_comp.replace(comp);
            }
            maybe_comp.unwrap()
        };
        let len_reveived = comp.data.len();

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

impl From<ReadQueue> for SyncReader {
    fn from(value: ReadQueue) -> Self {
        Self::new(value)
    }
}

impl From<SyncReader> for ReadQueue {
    fn from(value: SyncReader) -> Self {
        value.queue
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
        let comp = {
            let mut maybe_comp = block_for_timeout(fut, timeout);
            if maybe_comp.is_none() {
                self.queue.cancel_all(); // the only one
                if self.queue.pending() == 0 {
                    self.buf.replace(Vec::new());
                    return Err(Error::other("Unable to get the transfer result"));
                }
                let comp = block_on(self.queue.next_complete());
                maybe_comp.replace(comp);
            }
            maybe_comp.unwrap()
        };
        let len_sent = comp.data.actual_length();

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

impl From<WriteQueue> for SyncWriter {
    fn from(value: WriteQueue) -> Self {
        Self::new(value)
    }
}

impl From<SyncWriter> for WriteQueue {
    fn from(value: SyncWriter) -> Self {
        value.queue
    }
}
