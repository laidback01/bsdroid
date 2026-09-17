//! One MTP session over two bulk endpoints.
//!
//! The module does a command, then an optional data phase, then a response.
//! Each step has a deadline. A step that passes the deadline gives an error,
//! and the caller stops. See rule 2 in `docs/00-why.md`.
//!
//! # Why the crate exists
//!
//! `mtpprobe` and `mtpfs` both need this state machine. An earlier version
//! gave each program its own copy, and the two copies did not agree. The copy
//! in the filesystem read a short data phase and reported success, because it
//! never checked the payload against the length the device declared.
//!
//! One copy now serves both programs.
//!
//! # The crate writes nothing to the terminal
//!
//! A function reports what happened and the caller chooses the words. A
//! filesystem daemon has no terminal to write to, and a probe wants its own
//! wording. See [`SessionOpen`].

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use ptp_proto::{op, resp, Container, ContainerType, Header, ParseError, PayloadTooLarge};
use usb_freebsd::device::{MtpDevice, UsbError};

/// The smallest read buffer the crate accepts.
///
/// A container header holds 12 bytes, and a read must hold a header. The
/// value is far above that, because a read below one USB packet wastes a
/// transfer.
pub const READ_BUFFER_MIN: usize = 512;

/// The largest data phase the host accepts, in bytes.
///
/// A caller writes the payload to a writer, so the host does not hold the
/// payload in memory. The limit therefore guards against a damaged length
/// field, and not against a large file. A video of 8 GB is a real file.
pub const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024 * 1024;

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

/// The deadline for the first read of a drain, in milliseconds.
///
/// The value must be small. An endpoint with nothing on it costs this much
/// time at each session start.
const DRAIN_FIRST_MILLIS: u64 = 15;

/// The deadline for each read of a drain after the first, in milliseconds.
///
/// The host uses this value only after the device gives bytes.
const DRAIN_REST_MILLIS: u64 = 250;

/// How a session reads and writes.
///
/// A caller builds this one time and gives it to [`Session::new`]. An earlier
/// version read an environment variable inside each transfer, which cost a
/// lookup and a parse for every packet, and gave `bench` no way to change the
/// read size except to write to the environment of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// The deadline for one transfer.
    pub timeout: Duration,
    /// The size of one read, in bytes.
    pub read_buffer: usize,
    /// The count of payload bytes in one write to the device.
    pub write_chunk: usize,
    /// The largest packet the bulk endpoints accept, in bytes.
    ///
    /// A data phase that is a multiple of this count needs a packet of zero
    /// bytes at the end. The count is 512 at high speed, and 1024 at super
    /// speed.
    pub packet: u64,
}

/// The packet size of a USB 2.0 bulk endpoint, in bytes.
pub const USB2_PACKET: u64 = 512;

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            read_buffer: 64 * 1024,
            write_chunk: 512 * 1024,
            packet: USB2_PACKET,
        }
    }
}

impl Config {
    /// Corrects any value that would break a transfer.
    ///
    /// The function raises a read buffer below [`READ_BUFFER_MIN`], and it
    /// rounds a write chunk down to a whole number of USB packets.
    ///
    /// A write that is not a whole number of packets ends with a short packet.
    /// The device reads a short packet as the end of the data phase, and the
    /// rest of the file never arrives. An earlier version checked the chunk
    /// against 512, which is wrong on a super speed link where a packet holds
    /// 1024 bytes.
    pub fn normalised(mut self) -> Self {
        if self.packet == 0 {
            self.packet = USB2_PACKET;
        }
        if self.read_buffer < READ_BUFFER_MIN {
            self.read_buffer = READ_BUFFER_MIN;
        }

        // The first write holds the 12 byte header and some payload, so the
        // chunk must be larger than the header and a whole number of packets.
        let packet = self.packet as usize;
        let least = (ptp_proto::HEADER_LEN + 1).next_multiple_of(packet);
        if self.write_chunk < least {
            self.write_chunk = least;
        }
        self.write_chunk -= self.write_chunk % packet;

        self
    }
}

/// What one operation did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The response code the device gave.
    pub response_code: u16,
    /// The payload of the data phase. The field is empty if there was none.
    pub data: Vec<u8>,
    /// The parameters of the response container.
    ///
    /// These are not the payload of the data phase. `SendObjectInfo` answers
    /// with the storage, the parent and the handle of the new object here, and
    /// a caller that writes a file needs the handle.
    pub response_params: Vec<u32>,
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
        self.response_code == resp::OK
    }

    /// Reads the payload as a PTP array of `u32`.
    ///
    /// `GetStorageIDs` and `GetObjectHandles` both answer with this shape. An
    /// empty payload gives an empty list, because a device that holds nothing
    /// sends no array.
    pub fn as_u32_array(&self) -> Result<Vec<u32>, ParseError> {
        if self.data.is_empty() {
            return Ok(Vec::new());
        }
        ptp_proto::parse_u32_array(&self.data)
    }
}

/// What one streamed operation did.
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    /// The response code the device gave.
    pub response_code: u16,
    /// The parameters of the response container. See [`Outcome`].
    pub response_params: Vec<u32>,
    /// The count of payload bytes the host moved.
    pub bytes: u64,
    /// The time the operation took.
    pub elapsed: Duration,
    /// The count of reads the host did for the data phase.
    pub reads: usize,
}

impl StreamOutcome {
    /// Tells you if the device reported success.
    pub fn is_ok(&self) -> bool {
        self.response_code == resp::OK
    }
}

/// What [`Session::open_session_or_repair`] had to do.
///
/// The struct carries facts, and the caller chooses the words. `mtpprobe`
/// prints each field. `mtpfs` reads `wedged` and reports one message.
#[derive(Debug, Clone)]
pub struct SessionOpen {
    /// The answer to the `OpenSession` that the host kept.
    pub outcome: Outcome,
    /// The host met a session from an earlier program, and closed it.
    pub repaired: bool,
    /// What `CloseSession` answered during the repair.
    pub close_response: Option<u16>,
    /// The count of bytes the host dropped while it cleared the endpoints.
    pub dropped: u64,
    /// The device still reports an open session after the repair.
    ///
    /// The MTP service on the device holds the session and does not release
    /// it. A cable disconnect does not repair this state.
    pub wedged: bool,
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
        declared: u64,
        limit: u64,
    },
    /// The device stopped before the end of the data phase.
    Incomplete {
        step: &'static str,
        want: u64,
        got: u64,
    },
    /// The host cannot write the payload where the caller asked.
    Write {
        step: &'static str,
        source: std::io::Error,
    },
    /// The host cannot read the payload the caller promised the device.
    ///
    /// MTP gives the size before the bytes, and the device reads exactly that
    /// count. A reader that gives fewer bytes leaves the device waiting.
    ShortRead {
        step: &'static str,
        want: u64,
        got: u64,
        source: Option<std::io::Error>,
    },
    /// The payload does not fit the length field of a container.
    Build {
        step: &'static str,
        source: PayloadTooLarge,
    },
}

impl SessionError {
    /// The step that failed.
    pub fn step(&self) -> &'static str {
        match self {
            Self::Usb { step, .. }
            | Self::Parse { step, .. }
            | Self::Unexpected { step, .. }
            | Self::TooLarge { step, .. }
            | Self::Incomplete { step, .. }
            | Self::Write { step, .. }
            | Self::ShortRead { step, .. }
            | Self::Build { step, .. } => step,
        }
    }
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
            Self::Write { step, source } => {
                write!(f, "{step}: cannot write the payload: {source}")
            }
            Self::ShortRead {
                step,
                want,
                got,
                source,
            } => match source {
                Some(e) => write!(
                    f,
                    "{step}: the host promised {want} bytes and read {got}: {e}"
                ),
                None => write!(f, "{step}: the host promised {want} bytes and read {got}"),
            },
            Self::Build { step, source } => write!(f, "{step}: {source}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usb { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Write { source, .. } => Some(source),
            Self::ShortRead { source, .. } => source.as_ref().map(|e| e as _),
            Self::Build { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Says whether a data phase needs a packet of zero bytes at the end.
///
/// A device counts packets. A last packet that is full tells the device that
/// more bytes follow, and the device then waits. A packet of zero bytes tells
/// the device that the data phase is complete.
pub fn needs_zero_packet(total: u64, packet: u64) -> bool {
    packet != 0 && total % packet == 0
}

/// One MTP session.
///
/// The session owns the device, so it carries no lifetime. An earlier version
/// borrowed two channels from a device the caller held, and a caller that
/// wanted both in one struct had to launder the lifetimes with `unsafe`.
pub struct Session {
    device: MtpDevice,
    transaction: u32,
    config: Config,
    /// True while the host holds a session that it opened.
    ///
    /// A command that fails leaves the session open. The next program then
    /// meets a device that answers "session already open", and the fault looks
    /// like a broken device. [`Drop`] closes the session for this reason.
    session_open: bool,
}

impl Session {
    /// Starts a session over two open bulk channels.
    ///
    /// The function does not send `OpenSession`. A caller does that, so a
    /// caller can watch what the operation does.
    ///
    /// The function puts the endpoints in a known state first, and gives the
    /// count of bytes it dropped. A program that stopped in the middle of a
    /// transfer leaves the device with bytes to send.
    pub fn new(device: MtpDevice, config: Config) -> (Self, u64) {
        let mut s = Self {
            device,
            transaction: 0,
            config: config.normalised(),
            session_open: false,
        };
        let dropped = s.recover();
        (s, dropped)
    }

    /// Starts a session and skips the recovery step.
    ///
    /// The step costs [`DRAIN_FIRST_MILLIS`] on a device that works. A caller
    /// that measures the cost of a session start needs this function.
    pub fn new_without_recovery(device: MtpDevice, config: Config) -> Self {
        Self {
            device,
            transaction: 0,
            config: config.normalised(),
            session_open: false,
        }
    }

    /// The configuration the session uses.
    pub fn config(&self) -> Config {
        self.config
    }

    /// The speed of the USB link to the device.
    pub fn speed(&self) -> usb_freebsd::device::LinkSpeed {
        self.device.speed()
    }

    /// Reads what the device says it can do, from the BOS descriptor.
    ///
    /// A device that runs at high speed and reports super speed has a cable or
    /// a port that cannot do more. The answer does not change with the link.
    pub fn device_capabilities(
        &mut self,
    ) -> Result<usb_freebsd::descriptor::DeviceCapabilities, UsbError> {
        let timeout = self.config.timeout;
        self.device.capabilities(timeout)
    }

    /// Resets the device on the USB port.
    ///
    /// A caller uses this after it closes the session. The device starts
    /// again, and its node name can change.
    pub fn reset_device(&mut self) -> Result<(), UsbError> {
        self.device.reset()
    }

    /// Changes the size of one read.
    ///
    /// The benchmark of `mtpprobe` measures the rate at several read sizes.
    /// An earlier version wrote the size into the environment of the process
    /// for each row, which is not sound next to a thread.
    pub fn set_read_buffer(&mut self, bytes: usize) {
        self.config.read_buffer = bytes.max(READ_BUFFER_MIN);
    }

    /// Does one operation and reads the answer into memory.
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
            response_params: s.response_params,
            elapsed: s.elapsed,
            reads: s.reads,
        })
    }

    /// Does one operation, and writes the data phase to a writer.
    ///
    /// The function does not hold the payload in memory. A file of 10 GB
    /// therefore needs no memory of 10 GB. [`Session::operation`] calls this
    /// function with a vector, so the two functions share one path.
    pub fn operation_stream<W: Write>(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        out: &mut W,
    ) -> Result<StreamOutcome, SessionError> {
        let started = Instant::now();
        let tid = self.next_transaction();

        let command = ptp_proto::build_command(code, tid, params);
        self.write_all(step, &command)?;

        self.read_answer(step, started, out)
    }

    /// Does one operation that sends a payload the caller holds in memory.
    ///
    /// The steps:
    ///
    /// 1. Send the command container.
    /// 2. Send the data container, which holds the header and the payload.
    /// 3. Read the response container.
    pub fn operation_with_data(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        payload: &[u8],
    ) -> Result<Outcome, SessionError> {
        let mut cursor = payload;
        self.operation_sending(step, code, params, &mut cursor, payload.len() as u64)
    }

    /// Does one operation that sends a payload from a reader.
    ///
    /// The host holds one buffer, and not the whole payload. A copy of a file
    /// of 4 GB therefore needs no memory of 4 GB.
    ///
    /// `size` must be the count of bytes the reader gives. MTP needs the size
    /// before the bytes, and the device reads exactly that count.
    pub fn operation_sending<R: Read>(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        reader: &mut R,
        size: u64,
    ) -> Result<Outcome, SessionError> {
        let started = Instant::now();

        // The length field counts the header. A payload the field cannot count
        // is a fault the host reports before it sends anything.
        let total = ptp_proto::container_length(size)
            .map_err(|source| SessionError::Build { step, source })?;

        let tid = self.next_transaction();
        let command = ptp_proto::build_command(code, tid, params);
        self.write_all(step, &command)?;

        let chunk = self.config.write_chunk;
        let mut first = ptp_proto::build_data_header(code, tid, size)
            .map_err(|source| SessionError::Build { step, source })?;

        // The first write holds the header and as much payload as fits. The
        // chunk is a whole number of packets, and the header plus this count
        // fills it, so the write ends on a packet boundary.
        let room = (chunk - ptp_proto::HEADER_LEN) as u64;
        let want = room.min(size) as usize;
        let mut buf = vec![0u8; chunk];
        read_exact_or_short(reader, &mut buf[..want], step)?;
        first.extend_from_slice(&buf[..want]);
        self.write_all(step, &first)?;

        let mut sent = want as u64;

        // The bound comes from the size and the chunk, so the loop stops.
        let rounds = size / chunk as u64 + 4;
        for _ in 0..rounds {
            if sent >= size {
                break;
            }
            let want = (chunk as u64).min(size - sent) as usize;
            read_exact_or_short(reader, &mut buf[..want], step)?;
            self.write_all(step, &buf[..want])?;
            sent += want as u64;
        }

        if sent < size {
            return Err(SessionError::ShortRead {
                step,
                want: size,
                got: sent,
                source: None,
            });
        }

        // A data phase that ends on a packet boundary needs a packet of zero
        // bytes. The device counts packets, and a full last packet tells the
        // device that more bytes follow. The device then waits, and the
        // transfer stops.
        //
        // A file of 524276 bytes gives a data phase of 524288 bytes, which is
        // 1024 packets of 512 bytes. That file stopped a device before this
        // code existed.
        if needs_zero_packet(u64::from(total), self.config.packet) {
            self.write_all(step, &[])?;
        }

        // A device answers a send with a response and no data phase. The read
        // path handles both, so a device that does answer with data does not
        // confuse the host.
        let mut sink = Vec::new();
        let s = self.read_answer(step, started, &mut sink)?;
        Ok(Outcome {
            response_code: s.response_code,
            data: sink,
            response_params: s.response_params,
            elapsed: s.elapsed,
            reads: s.reads,
        })
    }

    /// Reads the answer of an operation: an optional data phase, then the
    /// response container.
    ///
    /// This function is the one place that joins many transfers into one
    /// payload, and the one place that checks the payload against the length
    /// the device declared.
    fn read_answer<W: Write>(
        &mut self,
        step: &'static str,
        started: Instant,
        out: &mut W,
    ) -> Result<StreamOutcome, SessionError> {
        let buf_size = self.config.read_buffer;
        let mut buf = vec![0u8; buf_size];
        let mut reads = 0usize;
        let mut payload_bytes = 0u64;

        let n = self.read_once(step, &mut buf)?;
        reads += 1;

        // `Container::parse` needs the whole container, and a large data phase
        // does not fit in one transfer. `Header::parse` reads the 12 byte
        // header alone.
        let first =
            Header::parse(&buf[..n]).map_err(|source| SessionError::Parse { step, source })?;

        let response = match first.kind {
            ContainerType::Data => {
                let declared = u64::from(first.length);
                if declared > MAX_DATA_BYTES {
                    return Err(SessionError::TooLarge {
                        step,
                        declared,
                        limit: MAX_DATA_BYTES,
                    });
                }

                // Write the payload of the first transfer.
                let start = n.min(ptp_proto::HEADER_LEN);
                out.write_all(&buf[start..n])
                    .map_err(|source| SessionError::Write { step, source })?;
                payload_bytes += (n - start) as u64;
                let mut have = n as u64;

                // The bound comes from the declared size and the read size, so
                // the count never stops a transfer the device can finish.
                let rounds = (declared / buf_size as u64 + 16).min(MAX_READ_ROUNDS);

                let mut complete = have >= declared;
                for _ in 0..rounds {
                    if complete {
                        break;
                    }
                    let more = self.read_once(step, &mut buf)?;
                    reads += 1;
                    if more == 0 {
                        break;
                    }
                    out.write_all(&buf[..more])
                        .map_err(|source| SessionError::Write { step, source })?;
                    payload_bytes += more as u64;
                    have += more as u64;
                    complete = have >= declared;
                }

                // A short data phase must be a fault, and not a short answer
                // that looks complete. An earlier copy of this loop in the
                // filesystem left this check out, and a truncated GetDeviceInfo
                // parsed into wrong values.
                if !complete {
                    return Err(SessionError::Incomplete {
                        step,
                        want: declared,
                        got: have,
                    });
                }

                // The response container follows the data container.
                let n2 = self.read_once(step, &mut buf)?;
                let c = Container::parse(&buf[..n2])
                    .map_err(|source| SessionError::Parse { step, source })?;
                if c.kind != ContainerType::Response {
                    return Err(SessionError::Unexpected {
                        step,
                        expected: ContainerType::Response,
                        got: c.kind,
                    });
                }
                (c.code, c.parameters())
            }
            ContainerType::Response => {
                // A response is small and always arrives whole, so the full
                // container parses here and gives the parameters.
                let c = Container::parse(&buf[..n])
                    .map_err(|source| SessionError::Parse { step, source })?;
                (c.code, c.parameters())
            }
            other => {
                return Err(SessionError::Unexpected {
                    step,
                    expected: ContainerType::Response,
                    got: other,
                })
            }
        };

        Ok(StreamOutcome {
            response_code: response.0,
            response_params: response.1,
            bytes: payload_bytes,
            elapsed: started.elapsed(),
            reads,
        })
    }

    /// Takes the next transaction id.
    fn next_transaction(&mut self) -> u32 {
        let tid = self.transaction;
        self.transaction = self.transaction.wrapping_add(1);
        tid
    }

    fn write_all(&mut self, step: &'static str, bytes: &[u8]) -> Result<(), SessionError> {
        let timeout = self.config.timeout;
        self.device
            .channels()
            .write
            .write(bytes, timeout)
            .map(|_| ())
            .map_err(|source| SessionError::Usb { step, source })
    }

    fn read_once(&mut self, step: &'static str, buf: &mut [u8]) -> Result<usize, SessionError> {
        let timeout = self.config.timeout;
        self.device
            .channels()
            .read
            .read(buf, timeout)
            .map_err(|source| SessionError::Usb { step, source })
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
        self.device.channels().write.clear_stall();
        self.device.channels().read.clear_stall();
        self.drain()
    }

    /// Reads and drops the bytes the device still holds for the host.
    ///
    /// The function gives the count of bytes it dropped. The loop stops at the
    /// first empty read, at the first fault, or at the round limit.
    pub fn drain(&mut self) -> u64 {
        let mut buf = vec![0u8; self.config.read_buffer];
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
        let mut deadline = Duration::from_millis(DRAIN_FIRST_MILLIS);
        let rest = Duration::from_millis(DRAIN_REST_MILLIS);

        for _ in 0..MAX_READ_ROUNDS {
            match self.device.channels().read.read(&mut buf, deadline) {
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
        let out = self.operation("OpenSession", op::OPEN_SESSION, &[id])?;
        if out.is_ok() || out.response_code == resp::SESSION_ALREADY_OPEN {
            self.session_open = true;
        }
        Ok(out)
    }

    /// Sends `CloseSession`.
    pub fn close_session(&mut self) -> Result<Outcome, SessionError> {
        let out = self.operation("CloseSession", op::CLOSE_SESSION, &[]);
        if let Ok(o) = &out {
            if o.is_ok() {
                self.session_open = false;
            }
        }
        out
    }

    /// Opens a session, and repairs the state an earlier session left.
    ///
    /// A program that stops without a `CloseSession` leaves a session open on
    /// the device. The device then answers `OpenSession` with 0x201e, which
    /// means the session is already open.
    ///
    /// The function closes the old session and opens a new one. A user
    /// therefore does not need to pull the cable after a program stops.
    ///
    /// The answer records what the function had to do. The function writes
    /// nothing, so the caller chooses the words.
    pub fn open_session_or_repair(&mut self, id: u32) -> Result<SessionOpen, SessionError> {
        let first = self.open_session(id)?;
        if first.response_code != resp::SESSION_ALREADY_OPEN {
            return Ok(SessionOpen {
                outcome: first,
                repaired: false,
                close_response: None,
                dropped: 0,
                wedged: false,
            });
        }

        let close_response = self.close_session().ok().map(|o| o.response_code);

        // The close can leave bytes on the way. Clear the endpoints before the
        // next command, or the next command meets a busy device.
        let dropped = self.recover();

        let second = self.open_session(id)?;
        let wedged = second.response_code == resp::SESSION_ALREADY_OPEN;

        Ok(SessionOpen {
            outcome: second,
            repaired: true,
            close_response,
            dropped,
            wedged,
        })
    }
}

impl Drop for Session {
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

/// Fills a buffer from a reader, and reports a short read with its cause.
fn read_exact_or_short<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    step: &'static str,
) -> Result<(), SessionError> {
    let mut done = 0;
    while done < buf.len() {
        match reader.read(&mut buf[done..]) {
            Ok(0) => {
                return Err(SessionError::ShortRead {
                    step,
                    want: buf.len() as u64,
                    got: done as u64,
                    source: None,
                })
            }
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                // An earlier version dropped the cause. A read fault on a
                // spool file then reached the user as a short read, with no
                // word about the disk.
                return Err(SessionError::ShortRead {
                    step,
                    want: buf.len() as u64,
                    got: done as u64,
                    source: Some(e),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_last_packet_needs_a_packet_of_zero_bytes() {
        // A file of 524276 bytes gives a data phase of 524288 bytes, which is
        // 1024 packets of 512 bytes. That size stopped a Samsung.
        assert!(needs_zero_packet(524_288, 512));
        assert!(needs_zero_packet(512, 512));
        assert!(needs_zero_packet(1024, 1024));
    }

    #[test]
    fn a_part_packet_needs_no_packet_of_zero_bytes() {
        // A file of 680397437 bytes gives 680397449, which is not a multiple.
        assert!(!needs_zero_packet(680_397_449, 512));
        assert!(!needs_zero_packet(1, 512));
        assert!(!needs_zero_packet(513, 512));
    }

    #[test]
    fn a_packet_size_of_zero_asks_for_no_packet() {
        assert!(!needs_zero_packet(512, 0));
    }

    /// A write chunk must be a whole number of USB packets.
    ///
    /// A write that ends part way through a packet ends with a short packet,
    /// and the device reads a short packet as the end of the data phase.
    #[test]
    fn a_write_chunk_rounds_down_to_whole_packets() {
        let c = Config {
            write_chunk: 1536,
            packet: 1024,
            ..Config::default()
        }
        .normalised();
        assert_eq!(c.write_chunk, 1024, "1536 is not a multiple of 1024");

        let c = Config {
            write_chunk: 1536,
            packet: 512,
            ..Config::default()
        }
        .normalised();
        assert_eq!(c.write_chunk, 1536, "1536 is a multiple of 512");
    }

    /// The first write holds a 12 byte header and some payload, so the chunk
    /// must leave room for both.
    #[test]
    fn a_write_chunk_always_holds_a_header_and_a_packet() {
        for packet in [512u64, 1024] {
            let c = Config {
                write_chunk: 1,
                packet,
                ..Config::default()
            }
            .normalised();
            assert!(c.write_chunk > ptp_proto::HEADER_LEN);
            assert_eq!(c.write_chunk % packet as usize, 0);
        }
    }

    #[test]
    fn a_read_buffer_never_falls_below_the_minimum() {
        let c = Config {
            read_buffer: 8,
            ..Config::default()
        }
        .normalised();
        assert_eq!(c.read_buffer, READ_BUFFER_MIN);
    }

    #[test]
    fn a_packet_size_of_zero_becomes_the_usb_2_size() {
        let c = Config {
            packet: 0,
            ..Config::default()
        }
        .normalised();
        assert_eq!(c.packet, USB2_PACKET);
    }

    #[test]
    fn the_default_configuration_survives_normalisation() {
        let d = Config::default();
        assert_eq!(d.normalised(), d);
    }

    #[test]
    fn a_short_reader_reports_the_count_it_gave() {
        let mut src = &b"1234"[..];
        let mut buf = [0u8; 8];
        let got = read_exact_or_short(&mut src, &mut buf, "SendObject");
        match got {
            Err(SessionError::ShortRead { want, got, .. }) => {
                assert_eq!(want, 8);
                assert_eq!(got, 4);
            }
            other => panic!("expected a short read, got {other:?}"),
        }
    }

    /// A read fault must reach the user with its cause.
    #[test]
    fn a_reader_that_fails_keeps_the_cause() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk gone"))
            }
        }

        let mut buf = [0u8; 4];
        match read_exact_or_short(&mut Failing, &mut buf, "SendObject") {
            Err(e @ SessionError::ShortRead { .. }) => {
                assert!(e.to_string().contains("disk gone"), "{e}");
            }
            other => panic!("expected a short read, got {other:?}"),
        }
    }

    #[test]
    fn an_error_names_the_step_that_failed() {
        let e = SessionError::Incomplete {
            step: "GetObject",
            want: 100,
            got: 40,
        };
        assert_eq!(e.step(), "GetObject");
        assert!(e.to_string().contains("GetObject"));
    }
}
