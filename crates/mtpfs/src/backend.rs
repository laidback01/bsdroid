//! The MTP session that answers a filesystem request.
//!
//! The module holds one session, and the session stays open while the mount
//! runs. A session costs about 46 milliseconds to open, and a filesystem does
//! many operations, so one session is right. See `docs/07-filesystem-design.md`.

use std::io::Read;
use std::rc::Rc;
use std::time::Duration;

use mtp_session::{Config, Session, SessionError};
use ptp_proto::{association, op, prop, resp, DeviceInfo, ObjectInfo, ParseError};
use usb_freebsd::device::{Backend as UsbBackend, UsbError};
use usb_freebsd::discover;

use crate::tree::{Entry, ROOT};

/// The deadline for one transfer.
///
/// The value is longer than the deadline `mtpprobe` uses. A filesystem asks
/// for a large read, and a slow device needs the time.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The size of one read, in bytes.
///
/// A larger buffer needs fewer reads for one data phase. FUSE asks for 131072
/// bytes, so a buffer above that size reads a whole answer in one call.
const READ_BUFFER: usize = 1024 * 1024;

/// The buffer size for a bulk endpoint, in bytes.
///
/// The value limits one transfer, so the value must not be below
/// `READ_BUFFER`.
const BULK_BUFFER: u32 = 1024 * 1024;

/// The count of bytes the host reads in advance, in bytes.
///
/// FUSE asks for 131072 bytes at a time. A copy of 450 MB then needs 3440
/// requests to the device, and each request costs about 18 milliseconds.
///
/// The host reads more than FUSE asks for, and keeps the rest. A later read of
/// the next part then needs no request. A copy reads a file from the start to
/// the end, so the next read almost always follows the last one.
const READ_AHEAD: usize = 4 * 1024 * 1024;

/// The count of bytes the host writes in one transfer.
///
/// The MTP driver of an Android kernel holds a buffer of 16384 bytes, in
/// `MTP_BULK_BUFFER_SIZE`.
const WRITE_CHUNK: usize = 512 * 1024;

/// What the user asked for, read one time at startup.
///
/// An earlier version read each variable inside the function that needed it.
/// `timeout()` therefore ran a lookup and a parse on every USB transfer, and
/// `write_chunk()` ran one for every chunk of a copy. A copy of 4 GB paid for
/// about 8000 of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// The deadline for one transfer.
    pub timeout: Duration,
    /// The count of payload bytes in one write to the device.
    pub write_chunk: usize,
    /// The host writes a line for each step it takes.
    pub debug: bool,
    /// The host uses the older listing, one request for each object.
    pub no_proplist: bool,
    /// The host sends a USB reset when the session closes.
    pub usb_reset: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout: TIMEOUT,
            write_chunk: WRITE_CHUNK,
            debug: false,
            no_proplist: false,
            usb_reset: false,
        }
    }
}

impl Settings {
    /// Reads the settings from the environment.
    ///
    /// | Variable               | What it changes                        |
    /// | ---------------------- | -------------------------------------- |
    /// | `BSDROID_TIMEOUT`      | the deadline, in seconds               |
    /// | `BSDROID_WRITE_CHUNK`  | the count of bytes in one write        |
    /// | `BSDROID_DEBUG`        | write a line for each step             |
    /// | `BSDROID_NO_PROPLIST`  | use the older listing                  |
    /// | `BSDROID_USB_RESET`    | reset the device when the session ends |
    ///
    /// `libmtp` holds a flag with the same purpose as `BSDROID_TIMEOUT`,
    /// which is `DEVICE_FLAG_LONG_TIMEOUT`.
    pub fn from_env() -> Self {
        let mut s = Self::default();

        if let Some(n) = env_number("BSDROID_TIMEOUT") {
            if n > 0 {
                s.timeout = Duration::from_secs(n);
            }
        }
        if let Some(n) = env_number("BSDROID_WRITE_CHUNK") {
            // `Config::normalised` rounds the count to whole USB packets, so
            // this code takes any count above zero.
            if n > 0 {
                s.write_chunk = n as usize;
            }
        }

        s.debug = env_flag("BSDROID_DEBUG");
        s.no_proplist = env_flag("BSDROID_NO_PROPLIST");
        s.usb_reset = env_flag("BSDROID_USB_RESET");
        s
    }
}

/// Reads a whole number from an environment variable.
fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// Reads a switch from an environment variable.
///
/// A variable that is absent is off. A variable that holds `0`, `no`, `false`
/// or `off` is also off, in any case of letters. Any other value is on, so
/// `BSDROID_DEBUG=1` works and so does `BSDROID_DEBUG=yes`.
///
/// An earlier version asked only whether the variable was set. A person who
/// wrote `BSDROID_USB_RESET=0` to turn the reset off turned it on, which is
/// the opposite of what the name and the value say. This was found when a
/// test harness did exactly that.
fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Err(_) => false,
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "no" | "false" | "off"
        ),
    }
}

/// A fault the filesystem reports.
#[derive(Debug)]
pub enum Error {
    /// No device gives an MTP interface.
    NoDevice,
    /// A USB operation failed.
    Usb(UsbError),
    /// A dataset does not parse.
    Parse(ParseError),
    /// A USB descriptor does not parse.
    Descriptor(usb_freebsd::descriptor::DescriptorError),
    /// The device answered with a fault code.
    Device { step: &'static str, code: u16 },
    /// The device gives no storage.
    NoStorage,
    /// The name of the device does not read as a node name.
    BadNode(String),
    /// The device the caller named gives no MTP interface.
    NamedDeviceNotFound(String),
    /// The device cannot read part of a file.
    ///
    /// A filesystem needs a partial read, because a read asks for an offset.
    NoPartialRead,
    /// The file is too large for the 32-bit size field of MTP.
    TooLarge { size: u64 },
    /// The fast listing gave an object with no name.
    BadListing { handle: u32 },
    /// An operation on the session failed.
    ///
    /// The session layer names the step and carries the cause, so this
    /// variant needs no fields of its own.
    Session(SessionError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => write!(
                f,
                "no device gives an MTP interface. Connect the cellphone, \
                 unlock the cellphone, and put the cellphone into file \
                 transfer mode"
            ),
            Self::Usb(e) => write!(f, "{e}"),
            Self::Parse(e) => write!(f, "{e}"),
            Self::Descriptor(e) => write!(f, "the descriptor does not parse: {e}"),
            Self::Device { step, code } => {
                write!(f, "{step}: the device answered with the code {code:#06x}")
            }
            Self::BadNode(n) => write!(
                f,
                "the name {n} does not read as a device node. A node name looks \
                 like ugen0.11, or /dev/ugen0.11"
            ),
            Self::NamedDeviceNotFound(n) => write!(
                f,
                "the device {n} gives no MTP interface. Run `mtpfs -l` for a list"
            ),
            Self::NoStorage => write!(
                f,
                "the device reports no storage. Unlock the cellphone, and put \
                 the cellphone into file transfer mode"
            ),
            Self::BadListing { handle } => {
                write!(f, "the object {handle} came back with no name")
            }
            Self::TooLarge { size } => write!(
                f,
                "the file holds {size} bytes, and MTP allows {} at most",
                ptp_proto::MAX_PAYLOAD_LEN
            ),
            Self::Session(e) => write!(f, "{e}"),
            Self::NoPartialRead => write!(
                f,
                "the device cannot read part of a file, and a filesystem needs \
                 that operation"
            ),
        }
    }
}

impl From<UsbError> for Error {
    fn from(e: UsbError) -> Self {
        Self::Usb(e)
    }
}
impl From<ParseError> for Error {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}
impl From<ptp_proto::PayloadTooLarge> for Error {
    fn from(e: ptp_proto::PayloadTooLarge) -> Self {
        Self::TooLarge { size: e.len }
    }
}
impl From<SessionError> for Error {
    fn from(e: SessionError) -> Self {
        Self::Session(e)
    }
}
impl From<usb_freebsd::descriptor::DescriptorError> for Error {
    fn from(e: usb_freebsd::descriptor::DescriptorError) -> Self {
        // An earlier version turned every descriptor fault into NoDevice. A
        // damaged descriptor and an absent cellphone then gave one message,
        // and the message named the wrong cause.
        Self::Descriptor(e)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usb(e) => Some(e),
            Self::Parse(e) => Some(e),
            Self::Session(e) => Some(e),
            Self::Descriptor(e) => Some(e),
            _ => None,
        }
    }
}

/// One device that gives an MTP interface.
pub struct DeviceEntry {
    /// The name of the node, such as `ugen0.11`.
    pub node: String,
    pub bus: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    /// The maker and the model, for a person to read.
    pub name: String,
}

/// Removes the folder from the list of the objects the folder holds.
///
/// A request with a depth of 1 gives the folder and the children of the
/// folder. A listing needs the children alone.
///
/// A Samsung answers this way for a request that names one property, and the
/// answer then holds 231 objects for a folder of 230 files. The extra object
/// is the folder.
pub fn drop_the_folder(
    records: Vec<ptp_proto::PropRecord>,
    folder: u32,
) -> Vec<ptp_proto::PropRecord> {
    records.into_iter().filter(|r| r.handle != folder).collect()
}

/// Splits a read that reaches the end of a file.
///
/// The answer holds the count for the first read, and a flag. The flag is true
/// when the caller must read one more byte at the end.
///
/// A Samsung stops when the last packet of a partial read holds exactly one
/// full packet, and the read reaches the end of the file. The workaround takes
/// one byte from the first read, and a second read takes that byte.
pub fn split_final_read(offset: u64, want: u64, file_size: u64, packet: u64) -> (u64, bool) {
    // The offset and the count both come from FUSE. A sum that wraps would
    // panic, because the release profile keeps the overflow checks on.
    let reaches_end = offset.saturating_add(want) >= file_size;
    if reaches_end && packet != 0 && want != 0 && want % packet == 0 {
        (want - 1, true)
    } else {
        (want, false)
    }
}

/// The bytes the host read in advance, for one object.
struct ReadCache {
    handle: u32,
    start: u64,
    data: Vec<u8>,
}

impl ReadCache {
    /// Gives the bytes for a read, when the cache holds them.
    ///
    /// The answer can be shorter than `want`. A read that crosses the end of
    /// the cache gets the part the cache holds, and the caller then asks the
    /// device for the rest. A caller that treats a short answer as the whole
    /// answer gives the kernel a short read, and the kernel takes a short read
    /// as the end of the file. See the note in [`Mtp::read_at`].
    fn get(&self, handle: u32, offset: u64, want: usize) -> Option<&[u8]> {
        if self.handle != handle || offset < self.start {
            return None;
        }
        let from = (offset - self.start) as usize;
        if from >= self.data.len() {
            return None;
        }
        let to = core::cmp::min(from + want, self.data.len());
        Some(&self.data[from..to])
    }
}

/// The session a mount holds open.
pub struct Mtp {
    /// The session owns the device and both transfers, and the device owns a
    /// share of the backend. There is no borrow between the fields, so this
    /// struct needs no `unsafe` and no lifetime.
    session: Session,
    settings: Settings,
    storage: u32,
    /// The operation code the device uses for a partial read.
    partial_read: u16,
    /// True while `GetObjectPropList` works on this device.
    ///
    /// The flag starts as the answer of the device. A fault turns the flag
    /// off, and the mount then uses the older way for the rest of the session.
    fast_list: bool,
    /// The largest packet the bulk endpoints accept, in bytes.
    ///
    /// A partial read that ends on a multiple of this count needs the
    /// workaround in [`split_final_read`]. The count is 512 at high speed, and
    /// 1024 at super speed.
    packet: u64,
    /// The bytes the host read in advance.
    cache: Option<ReadCache>,
    /// What the device says about itself.
    pub info: DeviceInfo,
}

impl Mtp {
    /// Lists each device that gives an MTP interface.
    ///
    /// The function gives the node name, the identifiers and the name of the
    /// maker, for a person to read.
    pub fn list_devices(settings: Settings) -> Result<Vec<DeviceEntry>, Error> {
        let backend = Rc::new(UsbBackend::new()?);

        Ok(discover::list(&backend, settings.timeout)
            .into_iter()
            .map(|mut f| DeviceEntry {
                node: f.node(),
                bus: f.bus,
                address: f.address,
                vendor_id: f.vendor_id,
                product_id: f.product_id,
                name: f.name(),
            })
            .collect())
    }

    /// Finds a device, opens a session, and reads what the device can do.
    ///
    /// `node` names one device, such as `ugen0.11`. A value of `None` takes
    /// the first device that gives an MTP interface.
    ///
    /// The function gives an error when the device cannot read part of a file,
    /// because a filesystem cannot work without that operation.
    pub fn open(node: Option<&str>, settings: Settings) -> Result<Self, Error> {
        let want = match node {
            Some(n) => Some(discover::parse_node(n).ok_or_else(|| Error::BadNode(n.to_string()))?),
            None => None,
        };
        // Every handle holds a share of the backend, so the backend lives for
        // as long as the device does. An earlier version boxed the backend and
        // the device and made two `&'static` references with `unsafe`.
        let backend = Rc::new(UsbBackend::new()?);

        let found = discover::find(&backend, want, settings.timeout).ok_or_else(|| match node {
            Some(n) => Error::NamedDeviceNotFound(n.to_string()),
            None => Error::NoDevice,
        })?;
        let (mut open, iface) = (found.device, found.iface);

        if open.kernel_driver_active(iface.interface_number) {
            let _ = open.detach_kernel_driver(iface.interface_number);
        }

        // The device and both transfers go into one owner, so no field of
        // `Mtp` borrows another field.
        let device = open.into_mtp(&iface, BULK_BUFFER)?;

        let config = Config {
            timeout: settings.timeout,
            read_buffer: READ_BUFFER,
            write_chunk: settings.write_chunk,
            packet: u64::from(iface.max_packet_size),
        }
        .normalised();

        // The session clears the endpoints and drops what an earlier program
        // left behind.
        let (session, dropped) = Session::new(device, config);
        if settings.debug && dropped > 0 {
            eprintln!("mtpfs: the device still held {dropped} bytes, and the host dropped them");
        }

        let mut m = Self {
            session,
            settings,
            storage: 0,
            partial_read: op::GET_PARTIAL_OBJECT,
            fast_list: false,
            packet: config.packet,
            cache: None,
            info: DeviceInfo::default(),
        };

        // A program that stopped without a CloseSession leaves a session open
        // on the device. The mount closes it and opens a new one, so a user
        // does not need to pull the cable.
        let opened = m.session.open_session_or_repair(1)?;
        if opened.wedged {
            eprintln!(
                "mtpfs: the device still reports an open session. Put the \
                 cellphone into charge only mode, and then into file transfer \
                 mode again"
            );
        } else if opened.repaired && m.settings.debug {
            eprintln!("mtpfs: closed a session that an earlier program left open");
        }
        if !opened.outcome.is_ok() {
            return Err(Error::Device {
                step: "OpenSession",
                code: opened.outcome.response_code,
            });
        }

        let raw = m.operation("GetDeviceInfo", op::GET_DEVICE_INFO, &[])?;
        m.info = DeviceInfo::parse(&raw)?;

        // A 64 bit offset is right for a large file, and both test devices
        // give the operation.
        // `BSDROID_NO_PROPLIST=1` turns the fast listing off. The switch gives
        // a way to compare the two ways on one device.
        m.fast_list = m.info.supports(op::GET_OBJECT_PROP_LIST) && !m.settings.no_proplist;

        m.partial_read = if m.info.supports(op::GET_PARTIAL_OBJECT_64) {
            op::GET_PARTIAL_OBJECT_64
        } else if m.info.supports(op::GET_PARTIAL_OBJECT) {
            op::GET_PARTIAL_OBJECT
        } else {
            return Err(Error::NoPartialRead);
        };

        m.storage = m.first_storage()?;
        Ok(m)
    }

    /// The name of the device, for a person to read.
    pub fn name(&self) -> String {
        format!("{} {}", self.info.manufacturer, self.info.model)
            .trim()
            .to_string()
    }

    /// Reads the objects a folder holds.
    ///
    /// The function takes the fast way when the device gives it, and the older
    /// way when the device does not. A fault in the fast way turns the fast way
    /// off for the rest of the session, and the older way then answers.
    pub fn list(&mut self, parent: u32) -> Result<Vec<Entry>, Error> {
        if self.fast_list {
            match self.list_by_properties(parent) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    self.fast_list = false;
                    if std::env::var_os("BSDROID_DEBUG").is_some() {
                        eprintln!(
                            "mtpfs: GetObjectPropList failed, and the older way follows: {e}"
                        );
                    }
                }
            }
        }
        self.list_one_at_a_time(parent)
    }

    /// Reads a folder with one request.
    ///
    /// `GetObjectPropList` gives the name, the size and the format of each
    /// object in a folder. The older way costs one request for each object, so
    /// a folder of 500 objects costs 501 requests.
    ///
    /// The root needs the handle 0 for this operation, and not 0xffffffff.
    /// The value 0xffffffff means every object on the device.
    fn list_by_properties(&mut self, parent: u32) -> Result<Vec<Entry>, Error> {
        const DEPTH_CHILDREN: u32 = 1;
        let arg = if parent == ROOT { 0 } else { parent };

        // The request names one property. A request for every property costs
        // more time than it saves, because the device then sends each date,
        // and each identifier of 128 bits, for each object.
        //
        // A measurement on a Samsung, for a folder of 230 objects:
        //
        //   one request for each object       0.73 s
        //   every property, one request       1.99 s
        //   three properties, three requests  0.21 s
        const WANTED: [u16; 3] = [
            ptp_proto::prop::OBJECT_FILE_NAME,
            ptp_proto::prop::OBJECT_SIZE,
            ptp_proto::prop::OBJECT_FORMAT,
        ];

        let mut entries = Vec::new();
        for code in WANTED {
            let data = self.operation(
                "GetObjectPropList",
                op::GET_OBJECT_PROP_LIST,
                &[arg, 0, code as u32, 0, DEPTH_CHILDREN],
            )?;
            entries.extend(ptp_proto::parse_object_prop_list(&data)?);
        }

        let raw = ptp_proto::fold_prop_list(&entries);
        if std::env::var_os("BSDROID_DEBUG").is_some() {
            eprintln!(
                "mtpfs: proplist folder arg={arg} gave {} records",
                raw.len()
            );
            for r in raw.iter().take(4) {
                eprintln!(
                    "  handle={} name={:?} format={:#06x}",
                    r.handle, r.name, r.format
                );
            }
        }
        let records = drop_the_folder(raw, arg);

        let mut out = Vec::with_capacity(records.len());
        for r in records {
            // A record with no name means the device answered with a shape
            // this project does not expect. The older way then answers.
            if r.name.is_empty() {
                return Err(Error::BadListing { handle: r.handle });
            }
            let is_dir = r.is_folder();
            out.push(Entry {
                handle: r.handle,
                parent,
                name: r.name,
                is_dir,
                size: r.size,
            });
        }
        Ok(out)
    }

    /// Reads a folder with one request for each object.
    ///
    /// The root needs the value 0xffffffff, and a folder needs its own handle.
    fn list_one_at_a_time(&mut self, parent: u32) -> Result<Vec<Entry>, Error> {
        let arg = if parent == ROOT {
            association::ROOT_ONLY
        } else {
            parent
        };
        let data = self.operation(
            "GetObjectHandles",
            op::GET_OBJECT_HANDLES,
            &[self.storage, 0, arg],
        )?;

        let handles: Vec<u32> = data
            .get(4..)
            .unwrap_or(&[])
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            let raw = self.operation("GetObjectInfo", op::GET_OBJECT_INFO, &[h])?;
            let info = match ObjectInfo::parse(&raw) {
                Ok(i) => i,
                Err(e) => {
                    // An earlier version dropped the object with no word. The
                    // file then vanished from the folder and nothing said why.
                    if self.settings.debug {
                        eprintln!("mtpfs: object {h} gives a dataset that does not parse: {e}");
                    }
                    continue;
                }
            };
            let is_dir = info.is_folder();
            out.push(Entry {
                handle: h,
                parent,
                name: info.filename,
                is_dir,
                size: u64::from(info.compressed_size),
            });
        }
        Ok(out)
    }

    /// Reads part of an object into a buffer.
    ///
    /// The function gives the count of bytes the device sent.
    ///
    /// A Samsung stops when the last packet of the answer holds exactly 512
    /// bytes. The function therefore asks for one byte less in that case, and
    /// a caller reads the last byte in a second call. See
    /// `docs/07-filesystem-design.md`.
    pub fn read_at(
        &mut self,
        handle: u32,
        offset: u64,
        want: usize,
        file_size: u64,
    ) -> Result<Vec<u8>, Error> {
        if want == 0 || offset >= file_size {
            return Ok(Vec::new());
        }

        // A read stops at the end of the file, and not after it.
        let want = core::cmp::min(want as u64, file_size - offset) as usize;
        let mut out: Vec<u8> = Vec::with_capacity(want);

        // The loop fills the answer. The cache gives the first part, and the
        // device gives the rest.
        //
        // An earlier version gave the caller the part the cache holds, and
        // stopped. A read that crosses the end of the cache then got fewer
        // bytes than the caller asked for. The kernel takes a short answer as
        // the end of the file, and a program that reads the file gets wrong
        // bytes after 4194304 bytes. `cp` reads on a boundary of 65536 bytes
        // and never crosses the end of the cache, so `cp` gave the right
        // bytes and `sha256` did not.
        while out.len() < want {
            let at = offset + out.len() as u64;
            let need = want - out.len();

            // The cache holds the answer for a read that follows an earlier
            // read.
            let hit = self
                .cache
                .as_ref()
                .and_then(|c| c.get(handle, at, need))
                .map(|b| b.to_vec());
            if let Some(bytes) = hit {
                out.extend_from_slice(&bytes);
                continue;
            }

            // Read more than the caller asks for, and keep the rest.
            let ahead = core::cmp::max(need as u64, READ_AHEAD as u64);
            let ask = core::cmp::min(ahead, file_size - at);
            let data = self.read_from_device(handle, at, ask, file_size)?;
            if data.is_empty() {
                break;
            }
            let take = core::cmp::min(need, data.len());
            out.extend_from_slice(&data[..take]);
            self.cache = Some(ReadCache {
                handle,
                start: at,
                data,
            });
        }

        Ok(out)
    }

    /// Asks the device for part of an object.
    fn read_from_device(
        &mut self,
        handle: u32,
        offset: u64,
        want: u64,
        file_size: u64,
    ) -> Result<Vec<u8>, Error> {
        // The fault needs two things at once: the read reaches the end of the
        // file, and the last packet holds exactly one USB packet.
        //
        // An earlier version took one byte from every read, and not only from
        // a read at the end. Every read then asked for a count that is not a
        // multiple of the packet size, and the device gave wrong bytes after
        // 131072 bytes. See docs/07-filesystem-design.md.
        let (first, short) = split_final_read(offset, want, file_size, self.packet);

        let mut data = self.partial_read(handle, offset, first)?;

        // The read above left one byte. A second read takes the byte, and the
        // count of one is not a multiple of the packet size, so the fault does
        // not happen again.
        //
        // An earlier version gave the answer of the first read to the caller.
        // A file whose last read is a multiple of the packet size then lost the
        // last byte. A file of 307200 bytes read back as 307199 bytes.
        if short && data.len() as u64 == first {
            let tail = self.partial_read(handle, offset + first, 1)?;
            data.extend_from_slice(&tail);
        }

        Ok(data)
    }

    /// Asks the device for a count of bytes, with no workaround.
    fn partial_read(&mut self, handle: u32, offset: u64, want: u64) -> Result<Vec<u8>, Error> {
        let params: Vec<u32> = if self.partial_read == op::GET_PARTIAL_OBJECT_64 {
            vec![
                handle,
                (offset & 0xffff_ffff) as u32,
                (offset >> 32) as u32,
                want as u32,
            ]
        } else {
            vec![handle, offset as u32, want as u32]
        };

        self.operation("GetPartialObject", self.partial_read, &params)
    }

    /// Writes a new object to the device.
    ///
    /// MTP needs two operations. `SendObjectInfo` gives the name, the folder
    /// and the size. `SendObject` gives the bytes.
    ///
    /// The size must be correct in the first operation, so a caller must hold
    /// the whole object before the call.
    ///
    /// The function gives the handle the device assigns.
    pub fn send_object(
        &mut self,
        parent: u32,
        name: &str,
        data: &[u8],
        is_folder: bool,
    ) -> Result<u32, Error> {
        // See the note in `send_object_stream`. The size goes in a 32 bit
        // field, and the data container counts the header as well.
        if data.len() as u64 > ptp_proto::MAX_PAYLOAD_LEN as u64 {
            return Err(Error::TooLarge {
                size: data.len() as u64,
            });
        }

        let info = ptp_proto::ObjectInfo::build_for_send(
            self.storage,
            parent,
            name,
            data.len() as u32,
            is_folder,
        );

        // The parent of an object in the root is 0xffffffff for this
        // operation, and 0 for a folder. The value is not the value that
        // GetObjectHandles takes.
        let parent_arg = if parent == ROOT { 0xffff_ffff } else { parent };

        let handle = self.announce_object(&info, parent_arg)?;

        // A folder needs no second operation.
        if is_folder {
            return Ok(handle);
        }

        self.sending_data("SendObject", op::SEND_OBJECT, &[], data)?;
        Ok(handle)
    }

    /// Sends `SendObjectInfo`, and gives the handle the device assigns.
    ///
    /// The host must announce an object before it sends the bytes. Both the
    /// buffered write and the streamed write start here.
    fn announce_object(&mut self, info: &[u8], parent_arg: u32) -> Result<u32, Error> {
        let params = self.sending_data(
            "SendObjectInfo",
            op::SEND_OBJECT_INFO,
            &[self.storage, parent_arg],
            info,
        )?;

        // The device answers with the storage, the parent and the new handle.
        params.get(2).copied().ok_or(Error::Device {
            step: "SendObjectInfo",
            code: resp::OK,
        })
    }

    /// Reads the size and the free space of the storage.
    pub fn storage_info(&mut self) -> Result<(u64, u64), Error> {
        let data = self.operation("GetStorageInfo", op::GET_STORAGE_INFO, &[self.storage])?;
        let info = ptp_proto::StorageInfo::parse(&data)?;
        Ok((info.max_capacity, info.free_space_in_bytes))
    }

    /// Gives an object a new name, in the same folder.
    pub fn rename_object(&mut self, handle: u32, name: &str) -> Result<(), Error> {
        // The value of the property is a PTP string, with no dataset around it.
        let mut payload = Vec::new();
        ptp_proto::push_string(&mut payload, name);

        self.sending_data(
            "SetObjectPropValue",
            op::SET_OBJECT_PROP_VALUE,
            &[handle, u32::from(prop::OBJECT_FILE_NAME)],
            &payload,
        )?;
        Ok(())
    }

    /// Moves an object to another folder.
    pub fn move_object(&mut self, handle: u32, new_parent: u32) -> Result<(), Error> {
        let parent_arg = if new_parent == ROOT {
            0xffff_ffff
        } else {
            new_parent
        };
        self.operation_that_changes(
            "MoveObject",
            op::MOVE_OBJECT,
            &[handle, self.storage, parent_arg],
        )?;
        Ok(())
    }

    /// Tells you if the device can give an object a new name.
    pub fn can_rename(&self) -> bool {
        self.info.supports(op::SET_OBJECT_PROP_VALUE)
    }

    /// Tells you if the device can move an object to another folder.
    pub fn can_move(&self) -> bool {
        self.info.supports(op::MOVE_OBJECT)
    }

    /// Writes a new object, and reads the bytes from a reader.
    ///
    /// The host does not hold the object in memory. A file of 4 GB therefore
    /// needs no memory of 4 GB.
    ///
    /// `size` must be the count of bytes the reader gives. MTP needs the size
    /// before the bytes, and the device reads exactly that count.
    pub fn send_object_stream<R: Read>(
        &mut self,
        parent: u32,
        name: &str,
        reader: &mut R,
        size: u64,
    ) -> Result<u32, Error> {
        // MTP gives 32 bits for the size of an object, and the length field of
        // the data container must also count the 12 byte header. The limit is
        // therefore `MAX_PAYLOAD_LEN`, and not `u32::MAX`.
        //
        // An earlier version compared against `u32::MAX`. A file of exactly
        // that size then passed the guard, and the length field wrapped to 11.
        if size > ptp_proto::MAX_PAYLOAD_LEN as u64 {
            return Err(Error::TooLarge { size });
        }

        let info =
            ptp_proto::ObjectInfo::build_for_send(self.storage, parent, name, size as u32, false);

        let parent_arg = if parent == ROOT { 0xffff_ffff } else { parent };

        let handle = self.announce_object(&info, parent_arg)?;

        let out =
            self.session
                .operation_sending("SendObject", op::SEND_OBJECT, &[], reader, size)?;
        self.cache = None;
        if !out.is_ok() {
            return Err(Error::Device {
                step: "SendObject",
                code: out.response_code,
            });
        }
        Ok(handle)
    }

    /// Removes an object from the device.
    pub fn delete_object(&mut self, handle: u32) -> Result<(), Error> {
        self.operation_that_changes("DeleteObject", op::DELETE_OBJECT, &[handle, 0])?;
        Ok(())
    }

    /// Does one operation, and gives the payload of the data phase.
    ///
    /// The function turns a fault code from the device into an error, because
    /// almost every caller wants the payload and nothing else.
    fn operation(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
    ) -> Result<Vec<u8>, Error> {
        let out = self.session.operation(step, code, params)?;
        if !out.is_ok() {
            return Err(Error::Device {
                step,
                code: out.response_code,
            });
        }
        Ok(out.data)
    }

    /// Does one operation that changes the device, and checks the answer.
    ///
    /// A change makes the listing of a folder old, so the function drops the
    /// read cache. An earlier version repeated that line at each call site,
    /// and one call site forgot it.
    fn operation_that_changes(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
    ) -> Result<Vec<u32>, Error> {
        let out = self.session.operation(step, code, params)?;
        self.cache = None;
        if !out.is_ok() {
            return Err(Error::Device {
                step,
                code: out.response_code,
            });
        }
        Ok(out.response_params)
    }

    /// Does one operation that sends a dataset and changes the device.
    ///
    /// The function gives the parameters of the response, which is where
    /// `SendObjectInfo` puts the handle of the new object.
    fn sending_data(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        payload: &[u8],
    ) -> Result<Vec<u32>, Error> {
        let out = self
            .session
            .operation_with_data(step, code, params, payload)?;
        self.cache = None;
        if !out.is_ok() {
            return Err(Error::Device {
                step,
                code: out.response_code,
            });
        }
        Ok(out.response_params)
    }

    /// Reads the identifier of the first storage.
    fn first_storage(&mut self) -> Result<u32, Error> {
        // A device can answer with no storage for a short time after a
        // connect. The loop asks again. See `docs/01-cold-start.md`.
        for attempt in 1..=10 {
            let data = self.operation("GetStorageIDs", op::GET_STORAGE_IDS, &[])?;
            if let Some(c) = data.get(4..8) {
                return Ok(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
            if attempt < 10 {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        Err(Error::NoStorage)
    }
}

impl Drop for Mtp {
    fn drop(&mut self) {
        // The PTP session must close before a USB reset reaches the port.
        //
        // This code runs before any field of `Mtp` drops, so `Session` has not
        // closed itself yet. The close therefore happens here. `Session` also
        // closes itself, and the second close does nothing once the first one
        // succeeded.
        let _ = self.session.close_session();

        // `BSDROID_USB_RESET=1` sends a USB reset when the session closes.
        //
        // `libmtp` holds the flag `DEVICE_FLAG_FORCE_RESET_ON_CLOSE` for this,
        // and the flag is on the entry for the MediaTek chip 0x0e8d:0x2008.
        // The comment of the flag says that some devices do not like a reset,
        // so `libmtp` does not reset by default either.
        //
        // A reset is not the PTP operation 0x66. A reset goes to the USB port,
        // and the device then starts again. The node name of the device can
        // change, so a caller who names a node must read the name again.
        if self.settings.usb_reset {
            if let Err(e) = self.session.reset_device() {
                eprintln!("mtpfs: the USB reset failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod flag_tests {
    use super::env_flag;

    /// The tests write to the environment of the process, so they must not run
    /// at the same time. One function holds every case for that reason.
    #[test]
    fn a_switch_reads_the_value_and_not_only_the_name() {
        let name = "BSDROID_TEST_FLAG";

        std::env::remove_var(name);
        assert!(!env_flag(name), "a variable that is absent is off");

        for off in ["0", "no", "false", "off", "OFF", "False", " 0 ", ""] {
            std::env::set_var(name, off);
            assert!(!env_flag(name), "{off:?} must read as off");
        }

        for on in ["1", "yes", "true", "on", "TRUE", "anything"] {
            std::env::set_var(name, on);
            assert!(env_flag(name), "{on:?} must read as on");
        }

        std::env::remove_var(name);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::ReadCache;

    /// A cache that holds 100 bytes of object 7, from offset 1000.
    fn cache() -> ReadCache {
        ReadCache {
            handle: 7,
            start: 1000,
            data: (0u8..100).collect(),
        }
    }

    #[test]
    fn a_read_inside_the_cache_gives_the_bytes() {
        let c = cache();
        assert_eq!(c.get(7, 1000, 10), Some(&(0u8..10).collect::<Vec<_>>()[..]));
        assert_eq!(c.get(7, 1050, 5), Some(&(50u8..55).collect::<Vec<_>>()[..]));
    }

    #[test]
    fn another_object_never_hits() {
        assert_eq!(cache().get(8, 1000, 10), None);
    }

    #[test]
    fn a_read_before_the_cache_never_hits() {
        // The cache holds no bytes below `start`, and a read that begins
        // earlier must go to the device.
        assert_eq!(cache().get(7, 999, 10), None);
        assert_eq!(cache().get(7, 0, 10), None);
    }

    #[test]
    fn a_read_after_the_cache_never_hits() {
        assert_eq!(cache().get(7, 1100, 10), None, "one byte past the end");
        assert_eq!(cache().get(7, 5000, 10), None);
    }

    /// This is the case that broke a copy before the loop in `read_at`.
    ///
    /// A read that begins inside the cache and ends past it gets the part the
    /// cache holds, and no more. An earlier version of `read_at` gave that
    /// short answer to the caller, so the kernel saw a short read and took it
    /// as the end of the file. A copy then gave wrong bytes after 4194304
    /// bytes. `cp` reads on a boundary of 65536 bytes and never crossed the
    /// end of the cache, so `cp` was right and `sha256` was not.
    #[test]
    fn a_read_that_crosses_the_end_gives_only_what_the_cache_holds() {
        let c = cache();
        let got = c
            .get(7, 1090, 50)
            .expect("the read begins inside the cache");
        assert_eq!(got.len(), 10, "the cache holds 10 bytes from 1090");
        assert_eq!(got, &(90u8..100).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn the_last_byte_of_the_cache_still_hits() {
        let c = cache();
        assert_eq!(c.get(7, 1099, 1), Some(&[99u8][..]));
        assert_eq!(c.get(7, 1099, 100), Some(&[99u8][..]), "and asks no more");
    }

    #[test]
    fn a_read_of_no_bytes_gives_no_bytes() {
        let c = cache();
        assert_eq!(c.get(7, 1000, 0), Some(&[][..]));
    }

    /// An empty cache must never report a hit, or `read_at` would loop.
    ///
    /// The loop in `read_at` continues while the answer is shorter than the
    /// count the caller asked for. A hit of zero bytes at the same offset
    /// would add nothing and go round again.
    #[test]
    fn an_empty_cache_never_hits() {
        let c = ReadCache {
            handle: 7,
            start: 1000,
            data: Vec::new(),
        };
        assert_eq!(c.get(7, 1000, 10), None);
        assert_eq!(c.get(7, 1001, 10), None);
    }
}

#[cfg(test)]
mod packet_tests {
    use super::{drop_the_folder, split_final_read};

    fn record(handle: u32, name: &str) -> ptp_proto::PropRecord {
        ptp_proto::PropRecord {
            handle,
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn the_folder_goes_out_of_its_own_listing() {
        let recs = vec![record(5, "Camera"), record(6, "a.jpg"), record(7, "b.jpg")];
        let kept = drop_the_folder(recs, 5);
        let names: Vec<&str> = kept.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a.jpg", "b.jpg"]);
    }

    #[test]
    fn the_root_keeps_each_object() {
        // The root uses the handle 0, and no object holds the handle 0.
        let recs = vec![record(1, "DCIM"), record(2, "Download")];
        assert_eq!(drop_the_folder(recs, 0).len(), 2);
    }

    #[test]
    fn a_read_to_the_end_on_a_packet_boundary_keeps_one_byte_back() {
        // A file of 307200 bytes read back as 307199 bytes before this code.
        assert_eq!(split_final_read(0, 307_200, 307_200, 512), (307_199, true));
        assert_eq!(split_final_read(0, 512, 512, 512), (511, true));
    }

    #[test]
    fn a_read_in_the_middle_keeps_no_byte_back() {
        assert_eq!(
            split_final_read(0, 4_194_304, 100_000_000, 512),
            (4_194_304, false)
        );
    }

    #[test]
    fn a_read_to_the_end_off_a_packet_boundary_keeps_no_byte_back() {
        assert_eq!(split_final_read(0, 307_201, 307_201, 512), (307_201, false));
    }

    #[test]
    fn a_super_speed_packet_is_1024_bytes() {
        assert_eq!(split_final_read(0, 4096, 4096, 1024), (4095, true));
        // The same read at 512 bytes also sits on a boundary.
        assert_eq!(split_final_read(0, 4096, 4096, 512), (4095, true));
        // A count of 1536 is a multiple of 512, and not of 1024.
        assert_eq!(split_final_read(0, 1536, 1536, 1024), (1536, false));
    }

    #[test]
    fn the_two_reads_add_up_to_the_count_the_caller_asked_for() {
        for want in [512u64, 1024, 4096, 307_200, 4_194_304] {
            let (first, short) = split_final_read(0, want, want, 512);
            let total = if short { first + 1 } else { first };
            assert_eq!(total, want, "a read of {want} bytes lost a byte");
        }
    }
}
