//! One MTP session over two bulk endpoints.
//!
//! The module does a command, then an optional data phase, then a response.
//! Each step has a deadline. A step that passes the deadline gives an error,
//! and the program stops. See rule 2 in `docs/00-why.md`.

use std::io::Write;
use std::time::{Duration, Instant};

use ptp_proto::{Container, ContainerType, Header, ParseError};
use usb_freebsd::device::{MtpChannels, UsbError};

/// Operation codes the probe uses.
///
/// The list holds only the codes the probe sends. A code goes in this list
/// when a step uses the code, and not before.
pub const OP_OPEN_SESSION: u16 = 0x1002;
pub const OP_CLOSE_SESSION: u16 = 0x1003;
pub const OP_GET_STORAGE_IDS: u16 = 0x1004;
pub const OP_GET_STORAGE_INFO: u16 = 0x1005;
pub const OP_GET_OBJECT_HANDLES: u16 = 0x1007;
pub const OP_GET_OBJECT_INFO: u16 = 0x1008;
pub const OP_GET_OBJECT: u16 = 0x1009;

/// Response code for success.
pub const RESP_OK: u16 = 0x2001;

/// The size of one read, in bytes.
const READ_BUFFER_DEFAULT: usize = 64 * 1024;

/// The smallest read buffer the code accepts.
///
/// A container header holds 12 bytes, and a read must hold a header.
const READ_BUFFER_MIN: usize = 512;

/// Gives the size of one read.
///
/// The environment variable `BSDROID_READ_BUFFER` changes the size. The
/// variable is a test hook, and not a setting for a user.
///
/// A small buffer makes a device answer in many transfers. The hook lets a
/// test cover the path that joins the transfers, with a payload that fits in
/// one transfer at the normal size.
fn read_buffer_size() -> usize {
    match std::env::var("BSDROID_READ_BUFFER") {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n >= READ_BUFFER_MIN => n,
            _ => READ_BUFFER_DEFAULT,
        },
        Err(_) => READ_BUFFER_DEFAULT,
    }
}

/// The largest data phase the probe accepts, in bytes.
///
/// A phone with many files answers `GetObjectHandles` with a large container.
/// The limit stops a damaged length field from filling the memory of the host.
const MAX_DATA_BYTES: usize = 64 * 1024 * 1024;

/// The largest count of reads for one data phase.
///
/// Rule 1 says that a loop must not depend on the device for the end
/// condition. See `docs/00-why.md`.
const MAX_READ_ROUNDS: usize = 8192;

/// What one operation did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The response code the device gave.
    pub response_code: u16,
    /// The payload of the data phase. The field is empty if there was none.
    pub data: Vec<u8>,
    /// The time the operation took.
    pub elapsed: Duration,
    /// The count of reads the host did for the data phase.
    ///
    /// A count above 1 shows that the host joined many transfers into one
    /// payload. The count is the evidence that the join works.
    pub reads: usize,
}

impl Outcome {
    /// Tells you if the device reported success.
    pub fn is_ok(&self) -> bool {
        self.response_code == RESP_OK
    }
}

/// What one streamed operation did.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    /// The response code the device gave.
    pub response_code: u16,
    /// The count of payload bytes the host wrote.
    pub bytes: u64,
    /// The time the operation took.
    pub elapsed: Duration,
    /// The count of reads the host did for the data phase.
    pub reads: usize,
}

impl StreamOutcome {
    /// Tells you if the device reported success.
    pub fn is_ok(&self) -> bool {
        self.response_code == RESP_OK
    }
}

/// A fault in an operation.
#[derive(Debug)]
pub enum SessionError {
    /// A transfer failed or passed the deadline.
    Usb {
        step: &'static str,
        source: UsbError,
    },
    /// The device sent bytes that do not parse.
    Parse {
        step: &'static str,
        source: ParseError,
    },
    /// The device sent a container of a kind the step does not expect.
    Unexpected {
        step: &'static str,
        expected: ContainerType,
        got: ContainerType,
    },
    /// The device declared a data phase larger than the host accepts.
    TooLarge {
        step: &'static str,
        declared: usize,
        limit: usize,
    },
    /// The device stopped before the end of the data phase.
    Incomplete {
        step: &'static str,
        want: usize,
        got: usize,
    },
    /// The host cannot write the payload where the caller asked.
    Write { step: &'static str, message: String },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usb { step, source } => write!(f, "{step}: {source}"),
            Self::Parse { step, source } => write!(f, "{step}: {source}"),
            Self::Unexpected {
                step,
                expected,
                got,
            } => write!(
                f,
                "{step}: the device sent {got:?}, and {expected:?} was due"
            ),
            Self::TooLarge {
                step,
                declared,
                limit,
            } => write!(
                f,
                "{step}: the device declared {declared} bytes, and the limit is {limit}"
            ),
            Self::Incomplete { step, want, got } => write!(
                f,
                "{step}: the device sent {got} bytes of the {want} it declared"
            ),
            Self::Write { step, message } => {
                write!(f, "{step}: cannot write the payload: {message}")
            }
        }
    }
}

/// One MTP session.
pub struct Session<'a> {
    channels: MtpChannels<'a>,
    transaction: u32,
    timeout: Duration,
}

impl<'a> Session<'a> {
    /// Starts a session over two open bulk channels.
    ///
    /// The session does not send `OpenSession`. A caller does that, so a caller
    /// can watch what the operation does.
    pub fn new(channels: MtpChannels<'a>, timeout: Duration) -> Self {
        Self {
            channels,
            transaction: 0,
            timeout,
        }
    }

    /// Does one operation and reads the answer.
    ///
    /// The steps:
    ///
    /// 1. Send the command container.
    /// 2. Read one container.
    /// 3. If the container is data, keep the payload and read one more.
    /// 4. Report the response code.
    pub fn operation(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
    ) -> Result<Outcome, SessionError> {
        let mut data = Vec::new();
        let s = self.operation_stream(step, code, params, &mut data)?;
        Ok(Outcome {
            response_code: s.response_code,
            data,
            elapsed: s.elapsed,
            reads: s.reads,
        })
    }

    /// Does one operation, and writes the data phase to a writer.
    ///
    /// The function does not hold the payload in memory. A file of 10 GB
    /// therefore needs no memory of 10 GB. `operation` calls this function with
    /// a vector, so the two functions share one path.
    pub fn operation_stream<W: Write>(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        out: &mut W,
    ) -> Result<StreamOutcome, SessionError> {
        let started = Instant::now();
        let tid = self.transaction;
        self.transaction = self.transaction.wrapping_add(1);

        let command = ptp_proto::build_command(code, tid, params);
        self.channels
            .write
            .write(&command, self.timeout)
            .map_err(|source| SessionError::Usb { step, source })?;

        let buf_size = read_buffer_size();
        let mut buf = vec![0u8; buf_size];
        let mut reads = 0usize;
        let mut payload_bytes = 0u64;

        // Read the first container.
        let n = self
            .channels
            .read
            .read(&mut buf, self.timeout)
            .map_err(|source| SessionError::Usb { step, source })?;
        reads += 1;

        // Read the header of the first container. `Container::parse` needs the
        // whole container, and a large data phase does not fit in one
        // transfer. `Header::parse` reads the 12 byte header alone.
        let first =
            Header::parse(&buf[..n]).map_err(|source| SessionError::Parse { step, source })?;

        let response = match first.kind {
            ContainerType::Data => {
                let declared = first.length as usize;
                if declared > MAX_DATA_BYTES {
                    return Err(SessionError::TooLarge {
                        step,
                        declared,
                        limit: MAX_DATA_BYTES,
                    });
                }

                // Write the payload of the first transfer.
                let start = core::cmp::min(n, ptp_proto::HEADER_LEN);
                out.write_all(&buf[start..n])
                    .map_err(|e| SessionError::Write {
                        step,
                        message: e.to_string(),
                    })?;
                payload_bytes += (n - start) as u64;
                let mut have = n;

                // The loop needs one round for each transfer. The bound comes
                // from the declared size, and the bound has a limit. A device
                // cannot hold the host here.
                let rounds = declared / buf_size + 4;
                let rounds = core::cmp::min(rounds, MAX_READ_ROUNDS);

                let mut complete = have >= declared;
                for _ in 0..rounds {
                    if complete {
                        break;
                    }
                    let more = self
                        .channels
                        .read
                        .read(&mut buf, self.timeout)
                        .map_err(|source| SessionError::Usb { step, source })?;
                    reads += 1;
                    if more == 0 {
                        break;
                    }
                    out.write_all(&buf[..more])
                        .map_err(|e| SessionError::Write {
                            step,
                            message: e.to_string(),
                        })?;
                    payload_bytes += more as u64;
                    have += more;
                    complete = have >= declared;
                }

                if !complete {
                    return Err(SessionError::Incomplete {
                        step,
                        want: declared,
                        got: have,
                    });
                }

                // The response container follows the data container.
                let n2 = self
                    .channels
                    .read
                    .read(&mut buf, self.timeout)
                    .map_err(|source| SessionError::Usb { step, source })?;
                Container::parse(&buf[..n2])
                    .map_err(|source| SessionError::Parse { step, source })?
                    .code
            }
            ContainerType::Response => first.code,
            other => {
                return Err(SessionError::Unexpected {
                    step,
                    expected: ContainerType::Response,
                    got: other,
                })
            }
        };

        Ok(StreamOutcome {
            response_code: response,
            bytes: payload_bytes,
            elapsed: started.elapsed(),
            reads,
        })
    }

    /// Sends `OpenSession` with a session id.
    pub fn open_session(&mut self, id: u32) -> Result<Outcome, SessionError> {
        self.operation("OpenSession", OP_OPEN_SESSION, &[id])
    }

    /// Sends `CloseSession`.
    pub fn close_session(&mut self) -> Result<Outcome, SessionError> {
        self.operation("CloseSession", OP_CLOSE_SESSION, &[])
    }
}

/// Gives a name for a response code.
pub fn response_name(code: u16) -> &'static str {
    match code {
        0x2001 => "OK",
        0x2002 => "General error",
        0x2003 => "Session not open",
        0x2004 => "Invalid transaction id",
        0x2005 => "Operation not supported",
        0x2006 => "Parameter not supported",
        0x2007 => "Incomplete transfer",
        0x2008 => "Invalid storage id",
        0x2009 => "Invalid object handle",
        0x200d => "Store full",
        0x2013 => "Store not available",
        0x201e => "Session already open",
        0x201f => "Transaction cancelled",
        _ => "unknown",
    }
}
