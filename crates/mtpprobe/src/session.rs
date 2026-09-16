//! One MTP session over two bulk endpoints.
//!
//! The module does a command, then an optional data phase, then a response.
//! Each step has a deadline. A step that passes the deadline gives an error,
//! and the program stops. See rule 2 in `docs/00-why.md`.

use std::time::{Duration, Instant};

use ptp_proto::{Container, ContainerType, ParseError};
use usb_freebsd::device::{MtpChannels, UsbError};

/// Operation codes the probe uses.
///
/// The list holds only the codes the probe sends. A code goes in this list
/// when a step uses the code, and not before.
pub const OP_OPEN_SESSION: u16 = 0x1002;
pub const OP_CLOSE_SESSION: u16 = 0x1003;
pub const OP_GET_STORAGE_IDS: u16 = 0x1004;
pub const OP_GET_STORAGE_INFO: u16 = 0x1005;

/// Response code for success.
pub const RESP_OK: u16 = 0x2001;

/// The largest response the probe reads, in bytes.
const READ_BUFFER: usize = 16 * 1024;

/// What one operation did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The response code the device gave.
    pub response_code: u16,
    /// The payload of the data phase. The field is empty if there was none.
    pub data: Vec<u8>,
    /// The time the operation took.
    pub elapsed: Duration,
}

impl Outcome {
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
        let started = Instant::now();
        let tid = self.transaction;
        self.transaction = self.transaction.wrapping_add(1);

        let command = ptp_proto::build_command(code, tid, params);
        self.channels
            .write
            .write(&command, self.timeout)
            .map_err(|source| SessionError::Usb { step, source })?;

        let mut data = Vec::new();
        let mut buf = vec![0u8; READ_BUFFER];

        // Read the first container.
        let n = self
            .channels
            .read
            .read(&mut buf, self.timeout)
            .map_err(|source| SessionError::Usb { step, source })?;

        let first =
            Container::parse(&buf[..n]).map_err(|source| SessionError::Parse { step, source })?;

        let response = match first.kind {
            ContainerType::Data => {
                data.extend_from_slice(first.payload);

                // A device may send a payload larger than one transfer. The
                // length field says how many bytes the whole container holds.
                let declared = first.length as usize;
                let mut have = n;
                // The loop has a bound, so a device cannot hold the host here.
                for _ in 0..64 {
                    if have >= declared {
                        break;
                    }
                    let more = self
                        .channels
                        .read
                        .read(&mut buf, self.timeout)
                        .map_err(|source| SessionError::Usb { step, source })?;
                    if more == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..more]);
                    have += more;
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

        Ok(Outcome {
            response_code: response,
            data,
            elapsed: started.elapsed(),
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
