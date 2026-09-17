//! The MTP session that answers a filesystem request.
//!
//! The module holds one session, and the session stays open while the mount
//! runs. A session costs about 46 milliseconds to open, and a filesystem does
//! many operations, so one session is right. See `docs/07-filesystem-design.md`.

use std::io::{Read, Write};
use std::time::Duration;

use ptp_proto::{op, Container, ContainerType, DeviceInfo, Header, ObjectInfo, ParseError};
use usb_freebsd::descriptor::{ConfigDescriptor, MtpInterface};
use usb_freebsd::device::{Backend as UsbBackend, MtpChannels, UsbError};

use crate::tree::{Entry, ROOT};

/// The deadline for one transfer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Gives the deadline for one transfer.
///
/// `BSDROID_TIMEOUT` holds a count of seconds, and gives a longer deadline for
/// a slow device. `libmtp` holds a flag with the same purpose,
/// `DEVICE_FLAG_LONG_TIMEOUT`.
fn timeout() -> Duration {
    match std::env::var("BSDROID_TIMEOUT") {
        Ok(v) => match v.parse::<u64>() {
            Ok(n) if n > 0 => Duration::from_secs(n),
            _ => TIMEOUT,
        },
        Err(_) => TIMEOUT,
    }
}

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
const WRITE_CHUNK: usize = 512 * 1024;

/// Gives the count of bytes in one write to the device.
///
/// `BSDROID_WRITE_CHUNK` holds the count, and gives a smaller write for a
/// device that cannot take a large one. The MTP driver of an Android kernel
/// holds a buffer of 16384 bytes, in `MTP_BULK_BUFFER_SIZE`.
fn write_chunk() -> usize {
    match std::env::var("BSDROID_WRITE_CHUNK") {
        Ok(v) => match v.parse::<usize>() {
            // The count must hold the header, and must be a whole number of
            // USB packets.
            Ok(n) if n > ptp_proto::HEADER_LEN && n % 512 == 0 => n,
            _ => WRITE_CHUNK,
        },
        Err(_) => WRITE_CHUNK,
    }
}

/// The deadline for the first read of a drain, in milliseconds.
const DRAIN_FIRST_MILLIS: u64 = 15;

/// The value that lists the root folder.
///
/// A device gives every object for 0x00000000, and the root folder for
/// 0xffffffff. See `docs/03-object-handles.md`.
const LIST_ROOT: u32 = 0xffff_ffff;

/// The packet size of a USB 2.0 bulk endpoint, in bytes.
///
/// A Samsung stops when the last packet of a partial read holds exactly this
/// count of bytes. See `docs/07-filesystem-design.md`.
const USB2_PACKET: u64 = 512;

/// Operation codes the filesystem sends.
const OP_OPEN_SESSION: u16 = 0x1002;
const OP_CLOSE_SESSION: u16 = 0x1003;
const OP_GET_STORAGE_IDS: u16 = 0x1004;
const OP_GET_OBJECT_HANDLES: u16 = 0x1007;
const OP_GET_OBJECT_INFO: u16 = 0x1008;
const OP_GET_DEVICE_INFO: u16 = 0x1001;
const OP_SEND_OBJECT_INFO: u16 = 0x100c;
const OP_SEND_OBJECT: u16 = 0x100d;
const OP_DELETE_OBJECT: u16 = 0x100b;
const OP_MOVE_OBJECT: u16 = 0x1019;
const OP_GET_STORAGE_INFO: u16 = 0x1005;
const OP_SET_OBJECT_PROP_VALUE: u16 = 0x9804;

/// The object property that holds the name of a file.
const PROP_OBJECT_FILE_NAME: u16 = 0xdc07;

/// Response code for success.
const RESP_OK: u16 = 0x2001;
/// Response code for a session that is already open.
const RESP_SESSION_ALREADY_OPEN: u16 = 0x201e;

/// A fault the filesystem reports.
#[derive(Debug)]
pub enum Error {
    /// No device gives an MTP interface.
    NoDevice,
    /// A USB operation failed.
    Usb(UsbError),
    /// A dataset does not parse.
    Parse(ParseError),
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
    /// The host read fewer bytes than the host promised the device.
    ShortRead {
        step: &'static str,
        want: u64,
        got: u64,
    },
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
            Self::ShortRead { step, want, got } => {
                write!(f, "{step}: the host promised {want} bytes and read {got}")
            }
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
impl From<usb_freebsd::descriptor::DescriptorError> for Error {
    fn from(_: usb_freebsd::descriptor::DescriptorError) -> Self {
        Self::NoDevice
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

/// Says whether a data phase needs a packet of zero bytes at the end.
///
/// A device counts packets. A last packet that is full tells the device that
/// more bytes follow, and the device then waits. A packet of zero bytes tells
/// the device that the data phase is complete.
pub fn needs_zero_packet(total: u64, packet: u64) -> bool {
    packet != 0 && total % packet == 0
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
    let reaches_end = offset + want >= file_size;
    if reaches_end && packet != 0 && want != 0 && want % packet == 0 {
        (want - 1, true)
    } else {
        (want, false)
    }
}

/// Fills a buffer from a reader, and reports a short read.
fn read_exact_or_short<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    step: &'static str,
) -> Result<(), Error> {
    let mut done = 0;
    while done < buf.len() {
        match reader.read(&mut buf[done..]) {
            Ok(0) => {
                return Err(Error::ShortRead {
                    step,
                    want: buf.len() as u64,
                    got: done as u64,
                })
            }
            Ok(n) => done += n,
            Err(_) => {
                return Err(Error::ShortRead {
                    step,
                    want: buf.len() as u64,
                    got: done as u64,
                })
            }
        }
    }
    Ok(())
}

/// Reads a node name, and gives the bus and the address.
///
/// The function takes `ugen0.11` and `/dev/ugen0.11`, which are the two forms
/// a person writes.
pub fn parse_node(s: &str) -> Option<(u8, u8)> {
    let s = s.strip_prefix("/dev/").unwrap_or(s);
    let s = s.strip_prefix("ugen")?;
    let (bus, addr) = s.split_once('.')?;
    Some((bus.parse().ok()?, addr.parse().ok()?))
}

/// The bytes the host read in advance, for one object.
struct ReadCache {
    handle: u32,
    start: u64,
    data: Vec<u8>,
}

impl ReadCache {
    /// Gives the bytes for a read, when the cache holds them.
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
    // The order of the fields sets the order of the drop. The channels must
    // close before the device, and the device before the backend.
    channels: MtpChannels<'static>,
    _device: Box<usb_freebsd::device::OpenDevice<'static>>,
    _backend: Box<UsbBackend>,
    transaction: u32,
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
    /// A data phase that is a multiple of this count needs a packet of zero
    /// bytes at the end. The count is 512 at high speed, and 1024 at super
    /// speed.
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
    pub fn list_devices() -> Result<Vec<DeviceEntry>, Error> {
        let backend = UsbBackend::new()?;
        let mut out = Vec::new();

        for d in backend.devices() {
            let (vid, pid) = d.ids();
            let bus = d.bus();
            let addr = d.address();

            let mut open = match d.open(4) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let raw = match open.config_descriptor_raw(timeout()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let cfg = match ConfigDescriptor::parse(&raw) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut names: Vec<(u8, Option<String>)> = Vec::new();
            for i in cfg
                .interfaces
                .iter()
                .filter(|i| i.is_vendor_mtp_candidate())
            {
                names.push((i.string_index, open.string_descriptor(i.string_index)));
            }
            let found = MtpInterface::find_with_names(&cfg, |idx| {
                names
                    .iter()
                    .find(|(i, _)| *i == idx)
                    .and_then(|(_, n)| n.clone())
            });
            if found.is_ok() {
                // String 1 holds the maker, and string 2 holds the model.
                let maker = open.string_descriptor(1).unwrap_or_default();
                let model = open.string_descriptor(2).unwrap_or_default();
                out.push(DeviceEntry {
                    node: format!("ugen{bus}.{addr}"),
                    bus,
                    address: addr,
                    vendor_id: vid,
                    product_id: pid,
                    name: format!("{maker} {model}").trim().to_string(),
                });
            }
        }
        Ok(out)
    }

    /// Finds a device, opens a session, and reads what the device can do.
    ///
    /// `node` names one device, such as `ugen0.11`. A value of `None` takes
    /// the first device that gives an MTP interface.
    ///
    /// The function gives an error when the device cannot read part of a file,
    /// because a filesystem cannot work without that operation.
    pub fn open(node: Option<&str>) -> Result<Self, Error> {
        let want = match node {
            Some(n) => Some(parse_node(n).ok_or_else(|| Error::BadNode(n.to_string()))?),
            None => None,
        };
        // The backend owns the devices, and the channels borrow the device.
        // A box gives each one a fixed address, and the code then makes the
        // lifetimes static by hand.
        let backend = Box::new(UsbBackend::new()?);
        let backend_ref: &'static UsbBackend = unsafe { &*(&*backend as *const UsbBackend) };

        let mut chosen = None;
        for d in backend_ref.devices() {
            // A caller that names a device gets that device, and no other.
            if let Some((bus, addr)) = want {
                if d.bus() != bus || d.address() != addr {
                    continue;
                }
            }
            let mut open = match d.open(4) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let raw = match open.config_descriptor_raw(timeout()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let cfg = match ConfigDescriptor::parse(&raw) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut names: Vec<(u8, Option<String>)> = Vec::new();
            for i in cfg
                .interfaces
                .iter()
                .filter(|i| i.is_vendor_mtp_candidate())
            {
                names.push((i.string_index, open.string_descriptor(i.string_index)));
            }
            let found = MtpInterface::find_with_names(&cfg, |idx| {
                names
                    .iter()
                    .find(|(i, _)| *i == idx)
                    .and_then(|(_, n)| n.clone())
            });
            if let Ok(iface) = found {
                chosen = Some((open, iface));
                break;
            }
        }

        let (open, iface) = match chosen {
            Some(v) => v,
            None => {
                return Err(match node {
                    Some(n) => Error::NamedDeviceNotFound(n.to_string()),
                    None => Error::NoDevice,
                })
            }
        };
        let mut device = Box::new(open);
        if device.kernel_driver_active(iface.interface_number) {
            let _ = device.detach_kernel_driver(iface.interface_number);
        }

        let device_ref: &'static mut usb_freebsd::device::OpenDevice<'static> =
            unsafe { &mut *(&mut *device as *mut _) };
        let channels = device_ref.open_mtp(&iface, BULK_BUFFER)?;

        let mut m = Self {
            channels,
            _device: device,
            _backend: backend,
            transaction: 0,
            storage: 0,
            partial_read: op::GET_PARTIAL_OBJECT,
            fast_list: false,
            packet: if iface.max_packet_size == 0 {
                USB2_PACKET
            } else {
                iface.max_packet_size as u64
            },
            cache: None,
            info: DeviceInfo {
                standard_version: 0,
                vendor_extension_id: 0,
                vendor_extension_version: 0,
                vendor_extension_desc: String::new(),
                operations_supported: Vec::new(),
                events_supported: Vec::new(),
                device_properties_supported: Vec::new(),
                capture_formats: Vec::new(),
                image_formats: Vec::new(),
                manufacturer: String::new(),
                model: String::new(),
                device_version: String::new(),
                serial_number: String::new(),
            },
        };

        m.drain();
        m.open_session()?;

        let raw = m.operation("GetDeviceInfo", OP_GET_DEVICE_INFO, &[])?;
        m.info = DeviceInfo::parse(&raw)?;

        // A 64 bit offset is right for a large file, and both test devices
        // give the operation.
        // `BSDROID_NO_PROPLIST=1` turns the fast listing off. The switch gives
        // a way to compare the two ways on one device.
        m.fast_list = m.info.supports(op::GET_OBJECT_PROP_LIST)
            && std::env::var_os("BSDROID_NO_PROPLIST").is_none();

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
        let arg = if parent == ROOT { LIST_ROOT } else { parent };
        let data = self.operation(
            "GetObjectHandles",
            OP_GET_OBJECT_HANDLES,
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
            let raw = self.operation("GetObjectInfo", OP_GET_OBJECT_INFO, &[h])?;
            let info = match ObjectInfo::parse(&raw) {
                Ok(i) => i,
                Err(_) => continue,
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

        let (code, resp) = self.operation_with_data(
            "SendObjectInfo",
            OP_SEND_OBJECT_INFO,
            &[self.storage, parent_arg],
            &info,
        )?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "SendObjectInfo",
                code,
            });
        }

        // The device answers with the storage, the parent and the new handle.
        let handle = resp.get(2).copied().ok_or(Error::Device {
            step: "SendObjectInfo",
            code,
        })?;

        // A folder needs no second operation.
        if is_folder {
            self.cache = None;
            return Ok(handle);
        }

        let (code, _) = self.operation_with_data("SendObject", OP_SEND_OBJECT, &[], data)?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "SendObject",
                code,
            });
        }
        self.cache = None;
        Ok(handle)
    }

    /// Reads the size and the free space of the storage.
    pub fn storage_info(&mut self) -> Result<(u64, u64), Error> {
        let data = self.operation("GetStorageInfo", OP_GET_STORAGE_INFO, &[self.storage])?;
        let info = ptp_proto::StorageInfo::parse(&data)?;
        Ok((info.max_capacity, info.free_space_in_bytes))
    }

    /// Gives an object a new name, in the same folder.
    pub fn rename_object(&mut self, handle: u32, name: &str) -> Result<(), Error> {
        // The value of the property is a PTP string, with no dataset around it.
        let mut payload = Vec::new();
        ptp_proto::push_ptp_string(&mut payload, name);

        let (code, _) = self.operation_with_data(
            "SetObjectPropValue",
            OP_SET_OBJECT_PROP_VALUE,
            &[handle, u32::from(PROP_OBJECT_FILE_NAME)],
            &payload,
        )?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "SetObjectPropValue",
                code,
            });
        }
        self.cache = None;
        Ok(())
    }

    /// Moves an object to another folder.
    pub fn move_object(&mut self, handle: u32, new_parent: u32) -> Result<(), Error> {
        let parent_arg = if new_parent == ROOT {
            0xffff_ffff
        } else {
            new_parent
        };
        let (code, _) = self.operation_code(
            "MoveObject",
            OP_MOVE_OBJECT,
            &[handle, self.storage, parent_arg],
        )?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "MoveObject",
                code,
            });
        }
        self.cache = None;
        Ok(())
    }

    /// Tells you if the device can give an object a new name.
    pub fn can_rename(&self) -> bool {
        self.info.supports(OP_SET_OBJECT_PROP_VALUE)
    }

    /// Tells you if the device can move an object to another folder.
    pub fn can_move(&self) -> bool {
        self.info.supports(OP_MOVE_OBJECT)
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

        let (code, resp) = self.operation_with_data(
            "SendObjectInfo",
            OP_SEND_OBJECT_INFO,
            &[self.storage, parent_arg],
            &info,
        )?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "SendObjectInfo",
                code,
            });
        }
        let handle = resp.get(2).copied().ok_or(Error::Device {
            step: "SendObjectInfo",
            code,
        })?;

        let (code, _) =
            self.operation_with_reader("SendObject", OP_SEND_OBJECT, &[], reader, size)?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "SendObject",
                code,
            });
        }
        self.cache = None;
        Ok(handle)
    }

    /// Does one operation, and reads the data phase from a reader.
    ///
    /// The function holds one buffer, and not the whole payload.
    fn operation_with_reader<R: Read>(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        reader: &mut R,
        size: u64,
    ) -> Result<(u16, Vec<u32>), Error> {
        let tid = self.transaction;
        self.transaction = self.transaction.wrapping_add(1);

        let command = ptp_proto::build_command(code, tid, params);
        self.channels.write.write(&command, timeout())?;

        // The first write holds the header, and the start of the payload.
        let total = ptp_proto::container_length(size)?;
        let mut first = ptp_proto::build_data_header(code, tid, size)?;

        let mut buf = vec![0u8; write_chunk()];
        let room = write_chunk() - ptp_proto::HEADER_LEN;
        let want = core::cmp::min(room as u64, size) as usize;
        read_exact_or_short(reader, &mut buf[..want], step)?;
        first.extend_from_slice(&buf[..want]);
        self.channels.write.write(&first, timeout())?;

        let mut sent = want as u64;

        // The loop has a bound that comes from the size, so the loop stops.
        let rounds = size / write_chunk() as u64 + 4;
        for _ in 0..rounds {
            if sent >= size {
                break;
            }
            let want = core::cmp::min(write_chunk() as u64, size - sent) as usize;
            read_exact_or_short(reader, &mut buf[..want], step)?;
            self.channels.write.write(&buf[..want], timeout())?;
            sent += want as u64;
        }

        if sent < size {
            return Err(Error::ShortRead {
                step,
                want: size,
                got: sent,
            });
        }

        // A data phase that ends on a packet boundary needs a packet of zero
        // bytes. The device counts packets, and a full last packet tells the
        // device that more bytes follow. The device then waits, and the
        // transfer stops.
        //
        // A file of 524276 bytes gives a data phase of 524288 bytes, which is
        // 1024 packets of 512 bytes. That file stopped the device before this
        // code.
        if needs_zero_packet(total as u64, self.packet) {
            self.channels.write.write(&[], timeout())?;
        }

        let mut rbuf = vec![0u8; READ_BUFFER];
        let n = self.channels.read.read(&mut rbuf, timeout())?;
        let c = Container::parse(&rbuf[..n])?;
        Ok((c.code, c.parameters()))
    }

    /// Removes an object from the device.
    pub fn delete_object(&mut self, handle: u32) -> Result<(), Error> {
        let (code, _) = self.operation_code("DeleteObject", OP_DELETE_OBJECT, &[handle, 0])?;
        if code != RESP_OK {
            return Err(Error::Device {
                step: "DeleteObject",
                code,
            });
        }
        self.cache = None;
        Ok(())
    }

    /// Does one operation that sends a data phase.
    ///
    /// The steps:
    ///
    /// 1. Send the command container.
    /// 2. Send the data container, which holds the header and the payload.
    /// 3. Read the response container.
    ///
    /// The function gives the response code and the parameters of the
    /// response.
    fn operation_with_data(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
        payload: &[u8],
    ) -> Result<(u16, Vec<u32>), Error> {
        let tid = self.transaction;
        self.transaction = self.transaction.wrapping_add(1);

        let command = ptp_proto::build_command(code, tid, params);
        self.channels.write.write(&command, timeout())?;

        // The data container holds the header and the payload. A large payload
        // goes in parts, because one transfer has a limit.
        let total = ptp_proto::container_length(payload.len() as u64)?;
        let mut first = ptp_proto::build_data_header(code, tid, payload.len() as u64)?;

        // The first write holds the header and as much payload as fits.
        let room = write_chunk() - ptp_proto::HEADER_LEN;
        let take = core::cmp::min(room, payload.len());
        first.extend_from_slice(&payload[..take]);
        self.channels.write.write(&first, timeout())?;

        let mut sent = take;
        while sent < payload.len() {
            let end = core::cmp::min(sent + write_chunk(), payload.len());
            self.channels.write.write(&payload[sent..end], timeout())?;
            sent = end;
        }

        // A data phase that ends on a packet boundary needs a packet of zero
        // bytes. The device counts packets, and a full last packet tells the
        // device that more bytes follow. The device then waits, and the
        // transfer stops.
        //
        // A file of 524276 bytes gives a data phase of 524288 bytes, which is
        // 1024 packets of 512 bytes. That file stopped the device before this
        // code.
        if needs_zero_packet(total as u64, self.packet) {
            self.channels.write.write(&[], timeout())?;
        }

        // Read the response.
        let mut buf = vec![0u8; READ_BUFFER];
        let n = self.channels.read.read(&mut buf, timeout())?;
        let c = Container::parse(&buf[..n])?;
        if c.kind != ContainerType::Response {
            return Err(Error::Device { step, code: c.code });
        }
        Ok((c.code, c.parameters()))
    }

    /// Sends `OpenSession`, and repairs a session an earlier program left.
    fn open_session(&mut self) -> Result<(), Error> {
        let (code, _) = self.operation_code("OpenSession", OP_OPEN_SESSION, &[1])?;
        if code == RESP_SESSION_ALREADY_OPEN {
            let _ = self.operation_code("CloseSession", OP_CLOSE_SESSION, &[]);
            self.drain();
            let (code, _) = self.operation_code("OpenSession", OP_OPEN_SESSION, &[1])?;
            if code != RESP_OK {
                return Err(Error::Device {
                    step: "OpenSession",
                    code,
                });
            }
        } else if code != RESP_OK {
            return Err(Error::Device {
                step: "OpenSession",
                code,
            });
        }
        Ok(())
    }

    /// Reads the identifier of the first storage.
    fn first_storage(&mut self) -> Result<u32, Error> {
        // A device can answer with no storage for a short time after a
        // connect. The loop asks again. See `docs/01-cold-start.md`.
        for attempt in 1..=10 {
            let data = self.operation("GetStorageIDs", OP_GET_STORAGE_IDS, &[])?;
            if let Some(c) = data.get(4..8) {
                return Ok(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
            if attempt < 10 {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        Err(Error::NoStorage)
    }

    /// Does one operation, and gives the payload.
    fn operation(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
    ) -> Result<Vec<u8>, Error> {
        let (response, data) = self.operation_code(step, code, params)?;
        if response != RESP_OK {
            return Err(Error::Device {
                step,
                code: response,
            });
        }
        Ok(data)
    }

    /// Does one operation, and gives the response code and the payload.
    fn operation_code(
        &mut self,
        step: &'static str,
        code: u16,
        params: &[u32],
    ) -> Result<(u16, Vec<u8>), Error> {
        let tid = self.transaction;
        self.transaction = self.transaction.wrapping_add(1);

        let command = ptp_proto::build_command(code, tid, params);
        self.channels.write.write(&command, timeout())?;

        let mut data = Vec::new();
        let mut buf = vec![0u8; READ_BUFFER];

        let n = self.channels.read.read(&mut buf, timeout())?;
        let first = Header::parse(&buf[..n])?;

        let response = match first.kind {
            ContainerType::Data => {
                let declared = first.length as usize;
                let start = core::cmp::min(n, ptp_proto::HEADER_LEN);
                data.extend_from_slice(&buf[start..n]);
                let mut have = n;

                let rounds = declared / READ_BUFFER + 16;
                for _ in 0..rounds {
                    if have >= declared {
                        break;
                    }
                    let more = self.channels.read.read(&mut buf, timeout())?;
                    if more == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..more]);
                    have += more;
                }

                let n2 = self.channels.read.read(&mut buf, timeout())?;
                Container::parse(&buf[..n2])?.code
            }
            ContainerType::Response => first.code,
            _ => {
                return Err(Error::Device {
                    step,
                    code: first.code,
                })
            }
        };
        Ok((response, data))
    }

    /// Reads and drops the bytes the device still holds.
    fn drain(&mut self) {
        let mut buf = vec![0u8; READ_BUFFER];
        let short = Duration::from_millis(DRAIN_FIRST_MILLIS);
        self.channels.write.clear_stall();
        self.channels.read.clear_stall();
        for _ in 0..64 {
            match self.channels.read.read(&mut buf, short) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}

impl Drop for Mtp {
    fn drop(&mut self) {
        let _ = self.operation_code("CloseSession", OP_CLOSE_SESSION, &[]);

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
        if std::env::var_os("BSDROID_USB_RESET").is_some() {
            if let Err(e) = self._device.reset() {
                eprintln!("mtpfs: the USB reset failed: {e}");
            }
        }
    }
}

/// Writes nothing. The type gives `operation_stream` a writer it can drop.
pub struct Sink;
impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod packet_tests {
    use super::{drop_the_folder, needs_zero_packet, split_final_read};

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
    fn a_full_last_packet_needs_a_packet_of_zero_bytes() {
        // A file of 524276 bytes gives a data phase of 524288 bytes, which is
        // 1024 packets of 512 bytes. That size stopped a Samsung.
        assert!(needs_zero_packet(524288, 512));
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
