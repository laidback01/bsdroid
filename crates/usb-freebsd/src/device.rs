//! USB device access over `libusb20`.
//!
//! This module holds every call into `libusb20`, and every `unsafe` block that
//! goes with one. The rest of the project reads and writes through the safe
//! interface here.
//!
//! One other place holds `unsafe`: the FUSE callbacks in `mtpfs`, which the C
//! library calls through a function pointer. That boundary cannot be safe.
//!
//! An earlier version of this comment said the module held the only unsafe
//! code in the project. It did not: `mtpfs` also made two `&'static`
//! references from a `Box` to work around the shape of this interface.
//! `OpenDevice::into_mtp` now gives an owner, and that `unsafe` is gone.
//!
//! # The rule this module exists to obey
//!
//! Every transfer takes a timeout, and the timeout goes to
//! `libusb20_tr_bulk_intr_sync`. That function does one transfer and returns.
//! The function does not need an event loop, so this module has no event loop.
//! See `docs/00-why.md`.

use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::rc::Rc;
use std::time::Duration;

use crate::descriptor::{DescriptorError, DeviceCapabilities, MtpInterface};
use crate::sys;

/// A limit on the device count.
///
/// The enumeration loop asks the library for the next device until the library
/// gives none. Rule 1 says that no loop depends on data from outside for the
/// end condition, so the loop also stops at this count.
const MAX_DEVICES: usize = 1024;

/// The largest configuration descriptor the module reads, in bytes.
const CONFIG_DESCRIPTOR_MAX: usize = 4096;

/// Standard request: get a descriptor.
const REQUEST_GET_DESCRIPTOR: u8 = 0x06;
/// Request type: device to host, standard, device.
const REQUEST_TYPE_IN_STANDARD_DEVICE: u8 = 0x80;
/// Descriptor type 0x02 is a configuration descriptor. The value sits in the
/// high byte of `wValue`.
const DESCRIPTOR_TYPE_CONFIGURATION: u16 = 0x0200;

/// The device reset request of the still imaging class.
///
/// The request puts the protocol state of the device back to the start.
const PTP_DEVICE_RESET_REQUEST: u8 = 0x66;

/// Descriptor type 0x0f is the BOS descriptor. The value sits in the high byte
/// of `wValue`.
const DESCRIPTOR_TYPE_BOS: u16 = 0x0f00;

/// The largest BOS descriptor the module reads, in bytes.
const BOS_DESCRIPTOR_MAX: usize = 256;

/// Request type: host to device, class, interface.
const REQUEST_TYPE_OUT_CLASS_INTERFACE: u8 = 0x21;

/// What a transfer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// The transfer finished. The device may still have sent fewer bytes than
    /// the host asked for.
    Completed,
    /// The transfer failed.
    Error,
    /// The transfer reached the timeout.
    ///
    /// This status is the reason the project uses `libusb20`. The
    /// `libusb-1.0` compatibility layer cannot report a timeout, because the
    /// layer does not return to the caller.
    TimedOut,
    /// The host cancelled the transfer.
    Cancelled,
    /// The endpoint is in a halt condition.
    Stall,
    /// The device is gone.
    NoDevice,
    /// The device sent more data than the host asked for.
    Overflow,
    /// A value the standard does not define.
    Other(u8),
}

impl TransferStatus {
    /// Converts a `libusb20` status value.
    pub fn from_wire(v: u8) -> Self {
        match v as u32 {
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_COMPLETED => Self::Completed,
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_ERROR => Self::Error,
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_TIMED_OUT => Self::TimedOut,
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_CANCELLED => Self::Cancelled,
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_STALL => Self::Stall,
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_NO_DEVICE => Self::NoDevice,
            sys::libusb20_transfer_status_LIBUSB20_TRANSFER_OVERFLOW => Self::Overflow,
            _ => Self::Other(v),
        }
    }

    /// Tells you if the transfer finished.
    pub fn is_ok(self) -> bool {
        self == Self::Completed
    }
}

impl fmt::Display for TransferStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::TimedOut => "timed out",
            Self::Cancelled => "cancelled",
            Self::Stall => "endpoint stall",
            Self::NoDevice => "no device",
            Self::Overflow => "overflow",
            Self::Other(v) => return write!(f, "unknown status {v}"),
        };
        f.write_str(s)
    }
}

/// The speed of the USB link to a device.
///
/// The speed limits the rate of a file copy. A host that reports a rate must
/// also report the speed, because a rate has no meaning without the speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSpeed {
    /// The library gives no speed.
    Unknown,
    /// USB 1.0 low speed, 1.5 Mbit each second.
    Low,
    /// USB 1.1 full speed, 12 Mbit each second.
    Full,
    /// USB 2.0 high speed, 480 Mbit each second.
    High,
    /// A speed that changes.
    Variable,
    /// USB 3.0 super speed, 5 Gbit each second.
    Super,
    /// USB 3.1 super speed plus, 10 Gbit each second.
    SuperPlus,
    /// A value the library gives that this code does not name.
    Other(u8),
}

impl LinkSpeed {
    /// Converts a `libusb20` speed value.
    pub fn from_wire(v: u8) -> Self {
        match v as u32 {
            sys::LIBUSB20_SPEED_UNKNOWN => Self::Unknown,
            sys::LIBUSB20_SPEED_LOW => Self::Low,
            sys::LIBUSB20_SPEED_FULL => Self::Full,
            sys::LIBUSB20_SPEED_HIGH => Self::High,
            sys::LIBUSB20_SPEED_VARIABLE => Self::Variable,
            sys::LIBUSB20_SPEED_SUPER => Self::Super,
            sys::LIBUSB20_SPEED_SUPER_PLUS => Self::SuperPlus,
            _ => Self::Other(v),
        }
    }

    /// The rate of the link, in bits each second.
    ///
    /// The value is the rate of the signal. A file copy never reaches the
    /// value, because the protocol needs part of the time.
    pub fn bits_per_second(self) -> Option<u64> {
        match self {
            Self::Low => Some(1_500_000),
            Self::Full => Some(12_000_000),
            Self::High => Some(480_000_000),
            Self::Super => Some(5_000_000_000),
            Self::SuperPlus => Some(10_000_000_000),
            Self::Unknown | Self::Variable | Self::Other(_) => None,
        }
    }

    /// An estimate of the rate a bulk transfer reaches, in bytes each second.
    ///
    /// The estimate comes from common measurements, and not from a standard.
    /// A host uses the estimate to tell a user whether the link limits the
    /// copy, or whether something else does.
    pub fn practical_bytes_per_second(self) -> Option<u64> {
        match self {
            Self::Low => Some(150_000),
            Self::Full => Some(1_000_000),
            Self::High => Some(42_000_000),
            Self::Super => Some(400_000_000),
            Self::SuperPlus => Some(900_000_000),
            Self::Unknown | Self::Variable | Self::Other(_) => None,
        }
    }

    /// A name for a person to read.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Low => "low, USB 1.0",
            Self::Full => "full, USB 1.1",
            Self::High => "high, USB 2.0",
            Self::Variable => "variable",
            Self::Super => "super, USB 3.0",
            Self::SuperPlus => "super plus, USB 3.1",
            Self::Other(_) => "a speed this code does not name",
        }
    }
}

impl fmt::Display for LinkSpeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self, self.bits_per_second()) {
            (_, Some(b)) => write!(f, "{} ({} Mbit each second)", self.name(), b / 1_000_000),
            // Keep the value, as `TransferStatus` does. A speed the code
            // cannot name is worth reporting with its number.
            (Self::Other(v), None) => write!(f, "{} ({v})", self.name()),
            (_, None) => f.write_str(self.name()),
        }
    }
}

/// A fault in a USB operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbError {
    /// The library gives no backend.
    NoBackend,
    /// The host cannot open the device. The value is the `libusb20` code.
    ///
    /// A code of -1 with a connected device often means a permission fault.
    /// The user must belong to the `operator` group.
    Open(i32),
    /// A control request failed. The value is the `libusb20` code.
    Control(i32),
    /// The host cannot open an endpoint. The value is the `libusb20` code.
    EndpointOpen { endpoint: u8, code: i32 },
    /// A transfer did not finish.
    Transfer {
        endpoint: u8,
        status: TransferStatus,
    },
    /// The host sent fewer bytes than the caller asked for.
    ShortWrite { want: usize, got: usize },
    /// The kernel will not release an interface.
    Detach { interface: u8, code: i32 },
    /// The host cannot reset the device.
    ///
    /// A reset needs root on FreeBSD. The kernel checks the privilege, and the
    /// `operator` group is not enough.
    Reset(i32),
    /// The configuration descriptor does not parse.
    Descriptor(DescriptorError),
    /// The timeout is too large for the library, which takes milliseconds in a
    /// `u32`.
    TimeoutTooLarge { millis: u128 },
}

impl From<DescriptorError> for UsbError {
    fn from(e: DescriptorError) -> Self {
        Self::Descriptor(e)
    }
}

impl fmt::Display for UsbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBackend => write!(f, "the library gives no USB backend"),
            Self::Open(c) => write!(
                f,
                "cannot open the device, code {c}. Check that you belong to the operator group"
            ),
            Self::Control(c) => write!(f, "the control request failed, code {c}"),
            Self::EndpointOpen { endpoint, code } => {
                write!(f, "cannot open endpoint {endpoint:#04x}, code {code}")
            }
            Self::Transfer { endpoint, status } => {
                write!(f, "the transfer on endpoint {endpoint:#04x} gave: {status}")
            }
            Self::ShortWrite { want, got } => {
                write!(
                    f,
                    "the host sent {got} bytes, and the caller asked for {want}"
                )
            }
            Self::Detach { interface, code } => write!(
                f,
                "the kernel will not release interface {interface}, code {code}"
            ),
            Self::Reset(c) => write!(
                f,
                "cannot reset the device, code {c}. A reset needs root on FreeBSD"
            ),
            Self::Descriptor(e) => write!(f, "the descriptor does not parse: {e}"),
            Self::TimeoutTooLarge { millis } => {
                write!(
                    f,
                    "the timeout of {millis} ms is larger than the library accepts"
                )
            }
        }
    }
}

impl std::error::Error for UsbError {}

/// Converts a duration to the millisecond count the library takes.
///
/// A timeout of 0 means no timeout in `libusb20`. The function raises 0 to 1,
/// so a caller cannot ask for an endless wait by accident. Rule 2 says that
/// every transfer has a timeout.
fn timeout_millis(d: Duration) -> Result<u32, UsbError> {
    let ms = d.as_millis();
    if ms > u32::MAX as u128 {
        return Err(UsbError::TimeoutTooLarge { millis: ms });
    }
    Ok(core::cmp::max(1, ms as u32))
}

/// The USB backend, which owns the list of devices.
pub struct Backend {
    be: *mut sys::libusb20_backend,
}

impl Backend {
    /// Opens the default backend for the platform.
    ///
    /// The function can fail, so the type has no `Default`.
    pub fn new() -> Result<Self, UsbError> {
        // SAFETY: the call takes no argument and gives a pointer or null.
        let be = unsafe { sys::libusb20_be_alloc_default() };
        if be.is_null() {
            return Err(UsbError::NoBackend);
        }
        Ok(Self { be })
    }

    /// Lists the devices the host can see.
    ///
    /// The backend owns every device, so each handle keeps the backend alive
    /// with an [`Rc`]. A caller therefore does not have to hold the backend
    /// in a variable for as long as it holds a device.
    ///
    /// The loop stops at `MAX_DEVICES`, so a library fault cannot make the loop
    /// run without end.
    pub fn devices(self: &Rc<Self>) -> Vec<DeviceHandle> {
        let mut out = Vec::new();
        let mut cur: *mut sys::libusb20_device = ptr::null_mut();

        for _ in 0..MAX_DEVICES {
            // SAFETY: `self.be` is a valid backend. The library gives the next
            // device, or null at the end of the list. The backend owns each
            // device, so this code does not free one.
            cur = unsafe { sys::libusb20_be_device_foreach(self.be, cur) };
            if cur.is_null() {
                break;
            }
            out.push(DeviceHandle {
                dev: cur,
                backend: Rc::clone(self),
            });
        }
        out
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // SAFETY: `self.be` comes from `libusb20_be_alloc_default`, and this
        // code runs one time.
        unsafe { sys::libusb20_be_free(self.be) };
    }
}

/// A device the host can see but has not opened.
///
/// The backend owns the device, so the handle holds a share of the backend
/// and keeps it alive.
pub struct DeviceHandle {
    dev: *mut sys::libusb20_device,
    backend: Rc<Backend>,
}

impl DeviceHandle {
    /// The bus number of the device.
    pub fn bus(&self) -> u8 {
        // SAFETY: `self.dev` is a valid device that the backend owns.
        unsafe { sys::libusb20_dev_get_bus_number(self.dev) }
    }

    /// The address of the device on the bus.
    pub fn address(&self) -> u8 {
        // SAFETY: see `bus`.
        unsafe { sys::libusb20_dev_get_address(self.dev) }
    }

    /// The speed of the link to the device.
    pub fn speed(&self) -> LinkSpeed {
        // SAFETY: `self.dev` is a valid device that the backend owns.
        LinkSpeed::from_wire(unsafe { sys::libusb20_dev_get_speed(self.dev) })
    }

    /// The vendor and product identifiers of the device.
    pub fn ids(&self) -> (u16, u16) {
        // SAFETY: the library gives a pointer to a decoded descriptor that the
        // device owns. The pointer stays valid while the device does.
        unsafe {
            let d = sys::libusb20_dev_get_device_desc(self.dev);
            if d.is_null() {
                return (0, 0);
            }
            ((*d).idVendor, (*d).idProduct)
        }
    }

    /// Opens the device.
    ///
    /// `transfer_slots` gives the count of transfers the host can hold open at
    /// one time. Two slots carry MTP, one for each direction.
    pub fn open(self, transfer_slots: u16) -> Result<OpenDevice, UsbError> {
        // SAFETY: `self.dev` is a valid device.
        let rc = unsafe { sys::libusb20_dev_open(self.dev, transfer_slots) };
        if rc != 0 {
            return Err(UsbError::Open(rc));
        }
        Ok(OpenDevice {
            dev: self.dev,
            backend: self.backend,
        })
    }
}

/// A device the host has opened.
///
/// The device holds a share of the backend, because the backend owns the
/// memory the device lives in.
pub struct OpenDevice {
    dev: *mut sys::libusb20_device,
    /// The backend that owns the memory `dev` points at.
    ///
    /// No code reads the field. The field exists so that the backend outlives
    /// the device, and dropping it is the whole job.
    #[allow(dead_code)]
    backend: Rc<Backend>,
}

impl OpenDevice {
    /// The speed of the link to the device.
    pub fn speed(&self) -> LinkSpeed {
        // SAFETY: `self.dev` is open.
        LinkSpeed::from_wire(unsafe { sys::libusb20_dev_get_speed(self.dev) })
    }

    /// Does one control request, and gives what the device sent.
    ///
    /// The three control requests of this module differ only in the request
    /// type, the request, the value, and the size of the buffer. An earlier
    /// version wrote the whole setup block three times, which meant three
    /// copies of an `unsafe` block to keep right.
    ///
    /// `capacity` is 0 for a request that carries no data.
    fn control_request(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        capacity: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, UsbError> {
        let ms = timeout_millis(timeout)?;
        let mut buf = vec![0u8; capacity];
        let mut actual: u16 = 0;

        // SAFETY: the setup struct is plain data. The format field must point
        // at the format the library gives, which is what LIBUSB20_INIT does in
        // C. The data buffer holds `capacity` bytes, and `wLength` says so. A
        // request with no data gives a null pointer and a length of 0.
        let rc = unsafe {
            let mut setup: sys::LIBUSB20_CONTROL_SETUP_DECODED = core::mem::zeroed();
            setup.LIBUSB20_CONTROL_SETUP_FORMAT = sys::LIBUSB20_CONTROL_SETUP_FORMAT.as_ptr();
            setup.bmRequestType = request_type;
            setup.bRequest = request;
            setup.wValue = value;
            setup.wIndex = index;
            setup.wLength = capacity as u16;

            let data = if capacity == 0 {
                ptr::null_mut()
            } else {
                buf.as_mut_ptr() as *mut c_void
            };

            sys::libusb20_dev_request_sync(self.dev, &mut setup, data, &mut actual, ms, 0)
        };

        if rc != 0 {
            return Err(UsbError::Control(rc));
        }
        buf.truncate(actual as usize);
        Ok(buf)
    }

    /// Reads the raw configuration descriptor with a control request.
    ///
    /// The function gives the bytes to the caller. The `descriptor` module then
    /// parses the bytes, and a test covers the parser with no device.
    pub fn config_descriptor_raw(&mut self, timeout: Duration) -> Result<Vec<u8>, UsbError> {
        self.control_request(
            REQUEST_TYPE_IN_STANDARD_DEVICE,
            REQUEST_GET_DESCRIPTOR,
            DESCRIPTOR_TYPE_CONFIGURATION,
            0,
            CONFIG_DESCRIPTOR_MAX,
            timeout,
        )
    }

    /// Reads the raw BOS descriptor with a control request.
    ///
    /// BOS means binary device object store. The descriptor says what the
    /// device can do, and the answer does not change with the speed of the
    /// link. A device that has no BOS descriptor answers with a fault, and
    /// that answer means the device runs at high speed at most.
    pub fn bos_descriptor_raw(&mut self, timeout: Duration) -> Result<Vec<u8>, UsbError> {
        self.control_request(
            REQUEST_TYPE_IN_STANDARD_DEVICE,
            REQUEST_GET_DESCRIPTOR,
            DESCRIPTOR_TYPE_BOS,
            0,
            BOS_DESCRIPTOR_MAX,
            timeout,
        )
    }

    /// Reads what the device says it can do.
    pub fn capabilities(&mut self, timeout: Duration) -> Result<DeviceCapabilities, UsbError> {
        let raw = self.bos_descriptor_raw(timeout)?;
        Ok(DeviceCapabilities::parse(&raw)?)
    }

    /// Reads a string descriptor by index.
    ///
    /// The function gives `None` when the device gives no string. A string
    /// index of 0 means the device gives no name, so the function gives `None`
    /// for index 0 without a request.
    pub fn string_descriptor(&mut self, index: u8) -> Option<String> {
        if index == 0 {
            return None;
        }
        let mut buf = [0u8; 256];

        // SAFETY: the buffer holds 256 bytes, and the length argument says so.
        // The library writes a C string, and the library ends the string.
        let rc = unsafe {
            sys::libusb20_dev_req_string_simple_sync(
                self.dev,
                index,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u16,
            )
        };
        if rc != 0 {
            return None;
        }

        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        match core::str::from_utf8(&buf[..end]) {
            Ok(s) if !s.is_empty() => Some(s.to_string()),
            _ => None,
        }
    }

    /// Sends a class request to an interface, with no data.
    ///
    /// The request type is 0x21, which means host to device, class, and
    /// interface.
    pub fn class_request_out(
        &mut self,
        request: u8,
        value: u16,
        interface: u8,
        timeout: Duration,
    ) -> Result<(), UsbError> {
        self.control_request(
            REQUEST_TYPE_OUT_CLASS_INTERFACE,
            request,
            value,
            u16::from(interface),
            0,
            timeout,
        )?;
        Ok(())
    }

    /// Resets the PTP state of the device.
    ///
    /// The still imaging class defines request 0x66, which is the device reset
    /// request. The request puts the protocol of the device in the state that
    /// follows a connect, and the request closes any open session.
    ///
    /// A program that stops in the middle of a data phase leaves the device in
    /// a state where the device answers no command. This request repairs that
    /// state, and the request needs no root.
    pub fn ptp_device_reset(&mut self, interface: u8, timeout: Duration) -> Result<(), UsbError> {
        self.class_request_out(PTP_DEVICE_RESET_REQUEST, 0, interface, timeout)
    }

    /// Reports whether a kernel driver holds an interface.
    pub fn kernel_driver_active(&mut self, interface: u8) -> bool {
        // SAFETY: `self.dev` is open.
        unsafe { sys::libusb20_dev_kernel_driver_active(self.dev, interface) == 0 }
    }

    /// Asks the kernel to release an interface.
    pub fn detach_kernel_driver(&mut self, interface: u8) -> Result<(), UsbError> {
        // SAFETY: `self.dev` is open.
        let rc = unsafe { sys::libusb20_dev_detach_kernel_driver(self.dev, interface) };
        if rc != 0 {
            // An earlier version gave `Open` here, whose message tells the
            // user to check the `operator` group. That advice does not apply
            // to a detach, which fails for other reasons.
            return Err(UsbError::Detach {
                interface,
                code: rc,
            });
        }
        Ok(())
    }

    /// Resets the device.
    ///
    /// A reset makes the device leave the bus and come back. The device is
    /// then in the state a user gets after a connect, so a reset clears a
    /// session that an earlier program left open.
    ///
    /// # A reset needs root
    ///
    /// An earlier version of this comment said that `usbconfig` needs root
    /// for a reset and that this call does not. A measurement says otherwise.
    /// On FreeBSD 15.1, for a user in the `operator` group:
    ///
    /// | Who runs it | Answer                             |
    /// | ----------- | ---------------------------------- |
    /// | operator    | -99, and the device stays on the bus |
    /// | root        | the device leaves and comes back in about 4600 ms |
    ///
    /// The code is `LIBUSB20_ERROR_OTHER`, and not `LIBUSB20_ERROR_ACCESS`,
    /// so the number alone does not name the cause. The measurement does.
    ///
    /// Membership of the `operator` group is enough for every other call in
    /// this module. A reset is the one operation that needs more.
    pub fn reset(&mut self) -> Result<(), UsbError> {
        // SAFETY: `self.dev` is open.
        let rc = unsafe { sys::libusb20_dev_reset(self.dev) };
        if rc != 0 {
            return Err(UsbError::Reset(rc));
        }
        Ok(())
    }

    /// Opens the two bulk endpoints of an MTP interface, and takes the device.
    ///
    /// The answer owns the device and both transfers, so no part of it borrows
    /// another part.
    ///
    /// An earlier version gave back channels that borrowed the device. A
    /// caller that wanted to hold both in one struct had to box the device,
    /// make a `&'static mut` from the box with `unsafe`, and then move the box
    /// into the same struct. That pattern is unsound: a `Box` carries a
    /// promise of unique access, and the move invalidates every pointer taken
    /// from it. It worked only because a channel holds a pointer that
    /// `libusb20` owns, and never a pointer into the box.
    pub fn into_mtp(self, iface: &MtpInterface, buffer_size: u32) -> Result<MtpDevice, UsbError> {
        // SAFETY: the two calls use two different slots, so the transfers do
        // not share state. `MtpDevice` owns the device, so each pointer stays
        // valid for as long as the channels do.
        let write = unsafe {
            let xfer = sys::libusb20_tr_get_pointer(self.dev, 0);
            open_transfer(xfer, iface.bulk_out, buffer_size)?
        };
        let read = unsafe {
            let xfer = sys::libusb20_tr_get_pointer(self.dev, 1);
            open_transfer(xfer, iface.bulk_in, buffer_size)?
        };

        Ok(MtpDevice {
            channels: MtpChannels { write, read },
            device: self,
        })
    }
}

/// Opens one transfer for an endpoint.
///
/// # Safety
///
/// `xfer` must be null, or a transfer that an open device owns.
unsafe fn open_transfer(
    xfer: *mut sys::libusb20_transfer,
    endpoint: u8,
    buffer_size: u32,
) -> Result<BulkChannel, UsbError> {
    if xfer.is_null() {
        return Err(UsbError::EndpointOpen { endpoint, code: -1 });
    }
    // SAFETY: the caller promises that `xfer` belongs to an open device. One
    // frame is enough, because a channel does one transfer at a time.
    let rc = unsafe { sys::libusb20_tr_open(xfer, buffer_size, 1, endpoint) };
    if rc != 0 {
        return Err(UsbError::EndpointOpen { endpoint, code: rc });
    }
    Ok(BulkChannel { xfer, endpoint })
}

/// A device with both MTP bulk endpoints open.
///
/// The type owns everything it needs: the two transfers, the device, and a
/// share of the backend that the device lives in. A caller can hold it in a
/// struct, move it, and return it from a function, with no lifetime to carry
/// and no `unsafe` at the call site.
pub struct MtpDevice {
    // The field order sets the order of the drop. The transfers must close
    // before the device, and the device before the backend.
    channels: MtpChannels,
    device: OpenDevice,
}

impl MtpDevice {
    /// The two bulk channels.
    pub fn channels(&mut self) -> &mut MtpChannels {
        &mut self.channels
    }

    /// The speed of the link to the device.
    pub fn speed(&self) -> LinkSpeed {
        self.device.speed()
    }

    /// Reads what the device says it can do, from the BOS descriptor.
    pub fn capabilities(&mut self, timeout: Duration) -> Result<DeviceCapabilities, UsbError> {
        self.device.capabilities(timeout)
    }

    /// Resets the device.
    ///
    /// A reset goes to the USB port, and the device then starts again. The
    /// node name can change, so a caller that named a node must read the name
    /// again.
    pub fn reset(&mut self) -> Result<(), UsbError> {
        self.device.reset()
    }
}

/// The two bulk channels of an MTP interface.
pub struct MtpChannels {
    /// The channel that sends data to the device.
    pub write: BulkChannel,
    /// The channel that reads data from the device.
    pub read: BulkChannel,
}

impl Drop for OpenDevice {
    fn drop(&mut self) {
        // SAFETY: `self.dev` is open, and this code runs one time. The backend
        // that owns the memory drops after this, because `self.backend` is a
        // field and a field drops after the body of `drop`.
        unsafe { sys::libusb20_dev_close(self.dev) };
    }
}

/// One bulk endpoint, open for transfers.
///
/// The type holds a pointer that `libusb20` owns, and it is reachable only
/// through the [`MtpDevice`] that owns the device. That ownership, and not a
/// lifetime parameter, is what keeps the pointer valid.
pub struct BulkChannel {
    xfer: *mut sys::libusb20_transfer,
    endpoint: u8,
}

impl BulkChannel {
    /// The address of the endpoint.
    pub fn endpoint(&self) -> u8 {
        self.endpoint
    }

    /// Sends bytes to the device, and stops at the timeout.
    ///
    /// The function copies the data, because the library takes a mutable
    /// pointer. A PTP command is small, so the copy costs little.
    pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, UsbError> {
        let mut owned = data.to_vec();
        let n = self.transfer(&mut owned, timeout)?;
        if n != data.len() {
            return Err(UsbError::ShortWrite {
                want: data.len(),
                got: n,
            });
        }
        Ok(n)
    }

    /// Reads bytes from the device, and stops at the timeout.
    ///
    /// A device may send fewer bytes than the buffer holds. The count of bytes
    /// the device sent is the return value.
    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, UsbError> {
        self.transfer(buf, timeout)
    }

    /// Does one transfer and waits for the end.
    ///
    /// This function is the heart of the project. One call does one transfer.
    /// The library honors the timeout, so the call always returns.
    fn transfer(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, UsbError> {
        let ms = timeout_millis(timeout)?;
        let mut actual: u32 = 0;

        // SAFETY: `self.xfer` is an open transfer. The buffer holds
        // `buf.len()` bytes, and the length argument says so. The call blocks
        // until the transfer ends or the timeout passes.
        let status = unsafe {
            sys::libusb20_tr_bulk_intr_sync(
                self.xfer,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut actual,
                ms,
            )
        };

        let status = TransferStatus::from_wire(status);
        if !status.is_ok() {
            return Err(UsbError::Transfer {
                endpoint: self.endpoint,
                status,
            });
        }
        Ok(actual as usize)
    }

    /// Clears a halt condition on the endpoint.
    pub fn clear_stall(&mut self) {
        // SAFETY: `self.xfer` is an open transfer.
        unsafe { sys::libusb20_tr_clear_stall_sync(self.xfer) };
    }
}

impl Drop for BulkChannel {
    fn drop(&mut self) {
        // SAFETY: `self.xfer` is open, and this code runs one time. The drain
        // waits for a transfer that is still in progress.
        unsafe {
            sys::libusb20_tr_drain(self.xfer);
            sys::libusb20_tr_close(self.xfer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_status_reads_every_defined_value() {
        assert_eq!(TransferStatus::from_wire(0), TransferStatus::Completed);
        assert_eq!(TransferStatus::from_wire(3), TransferStatus::Error);
        assert_eq!(TransferStatus::from_wire(4), TransferStatus::TimedOut);
        assert_eq!(TransferStatus::from_wire(5), TransferStatus::Cancelled);
        assert_eq!(TransferStatus::from_wire(6), TransferStatus::Stall);
        assert_eq!(TransferStatus::from_wire(7), TransferStatus::NoDevice);
        assert_eq!(TransferStatus::from_wire(8), TransferStatus::Overflow);
    }

    #[test]
    fn an_unknown_status_keeps_the_value() {
        assert_eq!(TransferStatus::from_wire(200), TransferStatus::Other(200));
    }

    #[test]
    fn only_completed_is_ok() {
        assert!(TransferStatus::Completed.is_ok());
        for bad in [
            TransferStatus::Error,
            TransferStatus::TimedOut,
            TransferStatus::Stall,
            TransferStatus::NoDevice,
        ] {
            assert!(!bad.is_ok(), "{bad} must not count as success");
        }
    }

    /// A timeout of 0 means "wait with no end" in `libusb20`. Rule 2 says that
    /// every transfer has a timeout, so the code must not pass 0.
    #[test]
    fn a_zero_timeout_becomes_one_millisecond() {
        assert_eq!(timeout_millis(Duration::from_millis(0)).unwrap(), 1);
        assert_eq!(timeout_millis(Duration::from_nanos(1)).unwrap(), 1);
    }

    #[test]
    fn a_normal_timeout_converts_to_milliseconds() {
        assert_eq!(timeout_millis(Duration::from_secs(5)).unwrap(), 5000);
        assert_eq!(timeout_millis(Duration::from_millis(250)).unwrap(), 250);
    }

    #[test]
    fn a_timeout_that_is_too_large_is_an_error() {
        let huge = Duration::from_secs(u64::from(u32::MAX));
        assert!(matches!(
            timeout_millis(huge),
            Err(UsbError::TimeoutTooLarge { .. })
        ));
    }
}
