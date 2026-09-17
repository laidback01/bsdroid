//! The MTP session that answers a filesystem request.
//!
//! The module holds one session, and the session stays open while the mount
//! runs. A session costs about 46 milliseconds to open, and a filesystem does
//! many operations, so one session is right. See `docs/07-filesystem-design.md`.

use std::io::Write;
use std::time::Duration;

use ptp_proto::{op, Container, ContainerType, DeviceInfo, Header, ObjectInfo, ParseError};
use usb_freebsd::descriptor::{ConfigDescriptor, MtpInterface};
use usb_freebsd::device::{Backend as UsbBackend, MtpChannels, UsbError};

use crate::tree::{Entry, ROOT};

/// The deadline for one transfer.
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
const WRITE_CHUNK: usize = 512 * 1024;

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
    /// The device cannot read part of a file.
    ///
    /// A filesystem needs a partial read, because a read asks for an offset.
    NoPartialRead,
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
            Self::NoStorage => write!(
                f,
                "the device reports no storage. Unlock the cellphone, and put \
                 the cellphone into file transfer mode"
            ),
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
impl From<usb_freebsd::descriptor::DescriptorError> for Error {
    fn from(_: usb_freebsd::descriptor::DescriptorError) -> Self {
        Self::NoDevice
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
    /// The bytes the host read in advance.
    cache: Option<ReadCache>,
    /// What the device says about itself.
    pub info: DeviceInfo,
}

impl Mtp {
    /// Finds a device, opens a session, and reads what the device can do.
    ///
    /// The function gives an error when the device cannot read part of a file,
    /// because a filesystem cannot work without that operation.
    pub fn open() -> Result<Self, Error> {
        // The backend owns the devices, and the channels borrow the device.
        // A box gives each one a fixed address, and the code then makes the
        // lifetimes static by hand.
        let backend = Box::new(UsbBackend::new()?);
        let backend_ref: &'static UsbBackend = unsafe { &*(&*backend as *const UsbBackend) };

        let mut chosen = None;
        for d in backend_ref.devices() {
            let mut open = match d.open(4) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let raw = match open.config_descriptor_raw(TIMEOUT) {
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

        let (open, iface) = chosen.ok_or(Error::NoDevice)?;
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
    /// The root needs the value 0xffffffff, and a folder needs its own handle.
    pub fn list(&mut self, parent: u32) -> Result<Vec<Entry>, Error> {
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

        // The cache holds the answer for a read that follows an earlier read.
        if let Some(c) = &self.cache {
            if let Some(bytes) = c.get(handle, offset, want) {
                return Ok(bytes.to_vec());
            }
        }

        // Read more than the caller asks for, and keep the rest.
        let ahead = core::cmp::max(want, READ_AHEAD) as u64;
        let ask = core::cmp::min(ahead, file_size - offset);
        let data = self.read_from_device(handle, offset, ask, file_size)?;

        let out = {
            let end = core::cmp::min(want, data.len());
            data[..end].to_vec()
        };
        self.cache = Some(ReadCache {
            handle,
            start: offset,
            data,
        });
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
        let mut want = want;

        // The fault needs two things at once: the read reaches the end of the
        // file, and the last packet holds exactly one USB 2.0 packet.
        //
        // An earlier version took one byte from every read, and not only from
        // a read at the end. Every read then asked for a count that is not a
        // multiple of the packet size, and the device gave wrong bytes after
        // 131072 bytes. See docs/07-filesystem-design.md.
        let reaches_end = offset + want >= file_size;
        if reaches_end && want % USB2_PACKET == 0 {
            want -= 1;
        }

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
        self.channels.write.write(&command, TIMEOUT)?;

        // The data container holds the header and the payload. A large payload
        // goes in parts, because one transfer has a limit.
        let header = ptp_proto::build(ptp_proto::ContainerType::Data, code, tid, &[]);
        let total = (ptp_proto::HEADER_LEN + payload.len()) as u32;

        let mut first = header.clone();
        first[0..4].copy_from_slice(&total.to_le_bytes());

        // The first write holds the header and as much payload as fits.
        let room = WRITE_CHUNK - ptp_proto::HEADER_LEN;
        let take = core::cmp::min(room, payload.len());
        first.extend_from_slice(&payload[..take]);
        self.channels.write.write(&first, TIMEOUT)?;

        let mut sent = take;
        while sent < payload.len() {
            let end = core::cmp::min(sent + WRITE_CHUNK, payload.len());
            self.channels.write.write(&payload[sent..end], TIMEOUT)?;
            sent = end;
        }

        // Read the response.
        let mut buf = vec![0u8; READ_BUFFER];
        let n = self.channels.read.read(&mut buf, TIMEOUT)?;
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
        self.channels.write.write(&command, TIMEOUT)?;

        let mut data = Vec::new();
        let mut buf = vec![0u8; READ_BUFFER];

        let n = self.channels.read.read(&mut buf, TIMEOUT)?;
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
                    let more = self.channels.read.read(&mut buf, TIMEOUT)?;
                    if more == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..more]);
                    have += more;
                }

                let n2 = self.channels.read.read(&mut buf, TIMEOUT)?;
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
