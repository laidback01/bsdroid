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
pub const OP_GET_DEVICE_INFO: u16 = 0x1001;
pub const OP_OPEN_SESSION: u16 = 0x1002;
pub const OP_CLOSE_SESSION: u16 = 0x1003;
pub const OP_GET_STORAGE_IDS: u16 = 0x1004;
pub const OP_GET_STORAGE_INFO: u16 = 0x1005;
pub const OP_GET_OBJECT_HANDLES: u16 = 0x1007;
pub const OP_GET_OBJECT_INFO: u16 = 0x1008;
pub const OP_GET_OBJECT: u16 = 0x1009;

/// Response code for success.
pub const RESP_OK: u16 = 0x2001;
/// Response code for a session that is already open.
pub const RESP_SESSION_ALREADY_OPEN: u16 = 0x201e;

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

/// The largest data phase the host accepts, in bytes.
///
/// The host writes the payload to a writer, and the host does not hold the
/// payload in memory. The limit therefore guards against a damaged length
/// field, and not against a large file. A video of 8 GB is a real file.
const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// The deadline for the first read of a drain, in milliseconds.
///
/// The value must be small. An endpoint with nothing on it costs this much
/// time at each session start.
const DRAIN_FIRST_MILLIS: u64 = 15;

/// The deadline for each read of a drain after the first, in milliseconds.
///
/// The host uses this value only after the device gives bytes.
const DRAIN_REST_MILLIS: u64 = 250;

/// The largest count of reads for one data phase.
///
/// Rule 1 says that a loop must not depend on the device for the end
/// condition. See `docs/00-why.md`.
///
/// The value must never stop a transfer that the device can finish. The count
/// of reads a transfer needs is the declared size divided by the read size.
/// The smallest read is [`READ_BUFFER_MIN`], so the largest honest count is
/// `MAX_DATA_BYTES / READ_BUFFER_MIN`.
///
/// An earlier version used 8192. A read of 4 KiB then stopped at
/// 8192 * 4096 bytes, which is 32 MiB, and a video of 39 MB failed. The limit
/// must come from the other limits, and not from a number somebody chose.
const MAX_READ_ROUNDS: u64 = MAX_DATA_BYTES / READ_BUFFER_MIN as u64;

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
    /// True when the host opened a session and did not close it.
    ///
    /// A command that fails leaves the session open. The next program then
    /// meets a device that answers "session already open", and the fault looks
    /// like a broken device. [`Drop`] closes the session for this reason.
    session_open: bool,
}

impl<'a> Session<'a> {
    /// Starts a session over two open bulk channels.
    ///
    /// The session does not send `OpenSession`. A caller does that, so a caller
    /// can watch what the operation does.
    pub fn new(channels: MtpChannels<'a>, timeout: Duration) -> Self {
        let mut s = Self {
            channels,
            transaction: 0,
            timeout,
            session_open: false,
        };
        // A program that stopped in the middle of a transfer can leave the
        // device with bytes to send. Start from a known state.
        //
        // BSDROID_NO_RECOVER skips the step. The variable answers one
        // question: what does the step cost on a device that works?
        let dropped = if std::env::var("BSDROID_NO_RECOVER").is_ok() {
            0
        } else {
            s.recover()
        };
        if dropped > 0 {
            println!("    the device still held {dropped} bytes, and the host dropped them");
        }
        s
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
                if declared as u64 > MAX_DATA_BYTES {
                    return Err(SessionError::TooLarge {
                        step,
                        declared,
                        limit: MAX_DATA_BYTES as usize,
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
                // The count comes from the declared size and the read size,
                // so the count never stops a transfer the device can finish.
                let rounds = (declared / buf_size + 16) as u64;
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

    /// Puts the two endpoints in a known state.
    ///
    /// A program that stops in the middle of a data phase leaves the device
    /// with bytes to send, and can leave an endpoint in a halt condition. The
    /// next program then finds a device that does not answer, and the fault
    /// looks like a broken device.
    ///
    /// The function clears the halt condition on both endpoints, and then
    /// reads and drops the bytes the device still holds. A new session starts
    /// with this function, so a user does not need to pull the cable.
    pub fn recover(&mut self) -> u64 {
        self.channels.write.clear_stall();
        self.channels.read.clear_stall();
        self.drain()
    }

    /// Reads and drops the bytes the device still holds for the host.
    ///
    /// A data phase that fails leaves the device with bytes to send. The next
    /// command then meets a device that is still sending, and the write to the
    /// device does not finish. The host must read the rest before the host
    /// sends a new command.
    ///
    /// The function gives the count of bytes it dropped. The loop stops at the
    /// first empty read, at the first fault, or at the round limit.
    pub fn drain(&mut self) -> u64 {
        let buf_size = read_buffer_size();
        let mut buf = vec![0u8; buf_size];
        let mut dropped = 0u64;

        // The first read decides whether the device holds anything. On a
        // device that works, the endpoint is empty, and the read waits for the
        // whole deadline and gives nothing.
        //
        // The first deadline is therefore very short. A device that holds
        // bytes answers at once, because the bytes are already there.
        //
        // An earlier version used 250 ms for every read. A session then cost
        // 290 ms in place of 29 ms, and the project reported the cost as a
        // property of the device. See docs/06-the-reset-that-breaks.md.
        let first = Duration::from_millis(DRAIN_FIRST_MILLIS);
        let rest = Duration::from_millis(DRAIN_REST_MILLIS);

        let mut deadline = first;
        for _ in 0..MAX_READ_ROUNDS {
            match self.channels.read.read(&mut buf, deadline) {
                Ok(0) => break,
                Ok(n) => {
                    dropped += n as u64;
                    // The device holds bytes, so a longer deadline is right
                    // for the rest.
                    deadline = rest;
                }
                Err(_) => break,
            }
        }
        dropped
    }

    /// Sends `OpenSession` with a session id.
    pub fn open_session(&mut self, id: u32) -> Result<Outcome, SessionError> {
        let out = self.operation("OpenSession", OP_OPEN_SESSION, &[id])?;
        if out.is_ok() || out.response_code == RESP_SESSION_ALREADY_OPEN {
            self.session_open = true;
        }
        Ok(out)
    }

    /// Sends `CloseSession`.
    pub fn close_session(&mut self) -> Result<Outcome, SessionError> {
        let out = self.operation("CloseSession", OP_CLOSE_SESSION, &[]);
        if let Ok(o) = &out {
            if o.is_ok() {
                self.session_open = false;
            }
        }
        out
    }

    /// Opens a session, and repairs the state of an earlier session.
    ///
    /// A program that stops without a `CloseSession` leaves a session open on
    /// the device. The device then answers `OpenSession` with 0x201e, which
    /// means the session is already open.
    ///
    /// The function closes the old session and opens a new one. A user
    /// therefore does not need to pull the cable after a program stops.
    pub fn open_session_or_repair(&mut self, id: u32) -> Result<Outcome, SessionError> {
        let first = self.open_session(id)?;
        if first.response_code != RESP_SESSION_ALREADY_OPEN {
            return Ok(first);
        }

        println!("    a session from an earlier program is open, and the host closes it");
        match self.close_session() {
            Ok(o) => println!(
                "    CloseSession gave {:#06x} {}",
                o.response_code,
                response_name(o.response_code)
            ),
            Err(e) => println!("    CloseSession failed: {e}"),
        }

        // The close can leave bytes on the way. Clear the endpoints before the
        // next command, or the next command meets a busy device.
        let dropped = self.recover();
        if dropped > 0 {
            println!("    the host dropped {dropped} more bytes");
        }

        let second = self.open_session(id)?;
        if second.response_code == RESP_SESSION_ALREADY_OPEN {
            report_wedged_service();
        }
        Ok(second)
    }
}

impl Drop for Session<'_> {
    /// Closes the session that the host opened.
    ///
    /// A command that fails returns early, and an early return leaves the
    /// session open on the device. The next program then meets a device that
    /// answers "session already open".
    ///
    /// This project caused that fault many times before this code existed. A
    /// session must close itself, and a caller must not need to remember.
    fn drop(&mut self) {
        if !self.session_open {
            return;
        }
        // The device can still hold bytes from a data phase that failed. Clear
        // the endpoints, or the close does not reach the device.
        let _ = self.recover();
        let _ = self.close_session();
    }
}

/// Tells the user how to repair the MTP service of the device.
///
/// The device answers `OpenSession` with "session already open", and the
/// device does not answer `CloseSession`. The MTP service of the device holds
/// a session, and the service does not release the session.
///
/// A cable disconnect does not repair this state. The USB connection starts
/// again, and the service on the device keeps running.
fn report_wedged_service() {
    println!();
    println!("    The device still reports an open session.");
    println!();
    println!("    What the host measured:");
    println!("      - OpenSession answers, and the answer is 0x201e.");
    println!("      - CloseSession gets no answer, and reaches the deadline.");
    println!("      - A cable disconnect does not change the answers.");
    println!();
    println!("    The MTP service on the phone holds the session. The USB");
    println!("    connection is not the cause, so a new cable connection does");
    println!("    not help.");
    println!();
    println!("    Try a device reset request first:");
    println!("      BSDROID_PTP_RESET=1 mtpprobe probe");
    println!();
    println!("    The request repairs some devices, and the request breaks");
    println!("    others. See docs/06-the-reset-that-breaks.md.");
    println!();
    println!("    If the request does not help, restart the MTP service on the");
    println!("    phone:");
    println!("      1. Open the USB notification.");
    println!("      2. Choose the option that holds the word `charge`, or the");
    println!("         option that says `No data transfer`.");
    println!("      3. Choose the option that holds the word `file` again.");
    println!();
    println!("    Each telephone names the options differently. The word in the");
    println!("    name is the part that does not change.");
    println!();
    println!("    A restart of the phone also works, and takes longer.");
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
