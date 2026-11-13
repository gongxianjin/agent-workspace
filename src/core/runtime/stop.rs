//! Opt-in process-local Runner control. A stop never proves an AO session exit.
use std::{
    io::{self, Read},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread::{self, JoinHandle},
};

/// Read only the literal first frame, byte by byte. A valid frame is exactly
/// `stop\n` or `stop\r\n`; later bytes remain unread and EOF is not required.
fn read_stop_frame(reader: &mut impl Read) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "expected first frame stop LF or stop CRLF");
    let mut byte = [0_u8; 1];
    for expected in b"stop" {
        reader.read_exact(&mut byte)?;
        if byte[0] != *expected {
            return Err(invalid());
        }
    }
    reader.read_exact(&mut byte)?;
    match byte[0] {
        b'\n' => Ok(()),
        b'\r' => {
            reader.read_exact(&mut byte)?;
            if byte[0] == b'\n' { Ok(()) } else { Err(invalid()) }
        }
        _ => Err(invalid()),
    }
}

pub(super) struct StdinStop {
    receiver: Receiver<io::Result<()>>,
    reader: Option<JoinHandle<()>>,
    outcome: Option<io::Result<()>>,
}

impl StdinStop {
    /// Start only for the explicit process-level `--stop-on-stdin` opt-in.
    pub(super) fn start() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("runner-stdin-control".into())
            .spawn(move || {
                let result = read_stop_frame(&mut io::stdin().lock());
                let accepted = result.is_ok();
                if sender.send(result).is_ok() && accepted {
                    eprintln!("runner stop requested; local supervision only");
                }
            })?;
        Ok(Self { receiver, reader: Some(reader), outcome: None })
    }

    pub(super) fn requested(&mut self) -> io::Result<bool> {
        if self.outcome.is_none() {
            let result = match self.receiver.try_recv() {
                Err(TryRecvError::Empty) => return Ok(false),
                Err(TryRecvError::Disconnected) => {
                    Err(io::Error::other("stdin control reader disconnected without a decision"))
                }
                Ok(result) => result,
            };
            self.outcome = Some(match self.reader.take().map(JoinHandle::join) {
                Some(Err(_)) => Err(io::Error::other("stdin control reader panicked")),
                _ => result,
            });
        }
        match self.outcome.as_ref().expect("control decision retained") {
            Ok(()) => Ok(true),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_stop_frame;
    use std::io::{self, Cursor, Read};

    #[test]
    fn exact_first_stop_frame_returns_without_consuming_tail_or_eof() {
        for (bytes, consumed) in [
            (b"stop\n".as_slice(), 5),
            (b"stop\r\n".as_slice(), 6),
            (b"stop\nJUNK".as_slice(), 5),
            (b"stop\r\nJUNK".as_slice(), 6),
        ] {
            let mut reader = RefuseAfterFrame { inner: Cursor::new(bytes), frame_bytes: consumed };
            read_stop_frame(&mut reader).expect("the first exact stop frame is valid");
            assert_eq!(reader.inner.position(), consumed);
        }
    }

    #[test]
    fn malformed_and_incomplete_first_frames_are_errors_not_successful_stops() {
        for bytes in [
            b"".as_slice(), b"s".as_slice(), b"stop".as_slice(), b"stop\r".as_slice(),
            b"STOP\n".as_slice(), b" stop\n".as_slice(), b"stop \n".as_slice(),
            b"stopJUNK\n".as_slice(), b"stop\rX".as_slice(), b"stop\rX\nstop\n".as_slice(),
            b"bad\nstop\n".as_slice(), b"\nstop\n".as_slice(), b"{\"stop\":true}\n".as_slice(),
            b"stop\0\n".as_slice(), b"stop\xff\n".as_slice(), b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".as_slice(),
        ] {
            let mut reader = Cursor::new(bytes);
            assert!(read_stop_frame(&mut reader).is_err(), "accepted {bytes:?}");
            assert!(reader.position() <= 6, "first-frame read was not bounded");
        }
    }

    #[test]
    fn read_failure_before_completion_is_propagated_without_retrying_other_errors() {
        for fail_after in 0..6 {
            let mut reader = FailingReader { bytes: Cursor::new(b"stop\r\n".as_slice()), fail_after, failed: false };
            let error = read_stop_frame(&mut reader).expect_err("unfinished control I/O must fail");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(reader.failed);
        }
    }

    #[test]
    fn fragmented_input_obeys_the_same_literal_frame_contract() {
        for fragment in 1..=8 {
            for (bytes, expected) in [
                (b"stop\nJUNK".as_slice(), true), (b"stop\r\nJUNK".as_slice(), true),
                (b"stopJUNK\n".as_slice(), false), (b"stop\rX\nstop\n".as_slice(), false),
                (b"stop\r".as_slice(), false),
            ] {
                let mut reader = FragmentedReader { bytes: Cursor::new(bytes), fragment };
                assert_eq!(read_stop_frame(&mut reader).is_ok(), expected, "fragment={fragment}, input={bytes:?}");
                assert!(reader.bytes.position() <= 6);
            }
        }
    }

    struct RefuseAfterFrame<'a> { inner: Cursor<&'a [u8]>, frame_bytes: u64 }
    impl Read for RefuseAfterFrame<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            assert!(self.inner.position() < self.frame_bytes, "a complete valid frame must not wait for EOF or read its tail");
            let available = (self.frame_bytes - self.inner.position()) as usize;
            let count = output.len().min(available);
            self.inner.read(&mut output[..count])
        }
    }

    struct FailingReader<'a> { bytes: Cursor<&'a [u8]>, fail_after: u64, failed: bool }
    impl Read for FailingReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            assert!(!self.failed, "non-interrupted I/O errors must not be retried");
            if self.bytes.position() == self.fail_after {
                self.failed = true;
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "owned test read failure"));
            }
            let count = output.len().min((self.fail_after - self.bytes.position()) as usize);
            self.bytes.read(&mut output[..count])
        }
    }

    struct FragmentedReader<'a> { bytes: Cursor<&'a [u8]>, fragment: usize }
    impl Read for FragmentedReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = output.len().min(self.fragment);
            self.bytes.read(&mut output[..count])
        }
    }
}
