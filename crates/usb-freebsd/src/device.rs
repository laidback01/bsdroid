//! USB device access over `libusb20`.
//!
//! This module holds the only unsafe code in the project. The module keeps the
//! unsafe code small, and gives a safe interface to the rest of the project.
//!
//! # The rule this module exists to obey
//!
//! Every transfer takes a timeout, and the timeout goes to
//! `libusb20_tr_bulk_intr_sync`. That function does one transfer and returns.
//! The function does not need an event loop, so this module has no event loop.
//! See `docs/00-why.md`.

use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::ptr;
use std::time::Duration;

use crate::descriptor::{ConfigDescriptor, DescriptorError, MtpInterface};
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
    /// The loop stops at `MAX_DEVICES`, so a library fault cannot make the loop
    /// run without end.
    pub fn devices(&self) -> Vec<DeviceHandle<'_>> {
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
                _marker: PhantomData,
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
/// The backend owns the device, so the handle borrows the backend.
pub struct DeviceHandle<'a> {
    dev: *mut sys::libusb20_device,
    _marker: PhantomData<&'a Backend>,
}

impl<'a> DeviceHandle<'a> {
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
    pub fn open(self, transfer_slots: u16) -> Result<OpenDevice<'a>, UsbError> {
        // SAFETY: `self.dev` is a valid device.
        let rc = unsafe { sys::libusb20_dev_open(self.dev, transfer_slots) };
        if rc != 0 {
            return Err(UsbError::Open(rc));
        }
        Ok(OpenDevice {
            dev: self.dev,
            _marker: PhantomData,
        })
    }
}

/// A device the host has opened.
pub struct OpenDevice<'a> {
    dev: *mut sys::libusb20_device,
    _marker: PhantomData<&'a Backend>,
}

impl<'a> OpenDevice<'a> {
    /// Reads the raw configuration descriptor with a control request.
    ///
    /// The function gives the bytes to the caller. The `descriptor` module then
    /// parses the bytes, and a test covers the parser with no device.
    pub fn config_descriptor_raw(&mut self, timeout: Duration) -> Result<Vec<u8>, UsbError> {
        let ms = timeout_millis(timeout)?;
        let mut buf = vec![0u8; CONFIG_DESCRIPTOR_MAX];

        // SAFETY: the setup struct is plain data. The format field must point
        // at the format the library gives, which is what LIBUSB20_INIT does in
        // C. The data buffer holds `buf.len()` bytes, and `wLength` says so.
        let mut actual: u16 = 0;
        let rc = unsafe {
            let mut setup: sys::LIBUSB20_CONTROL_SETUP_DECODED = core::mem::zeroed();
            setup.LIBUSB20_CONTROL_SETUP_FORMAT = sys::LIBUSB20_CONTROL_SETUP_FORMAT.as_ptr();
            setup.bmRequestType = REQUEST_TYPE_IN_STANDARD_DEVICE;
            setup.bRequest = REQUEST_GET_DESCRIPTOR;
            setup.wValue = DESCRIPTOR_TYPE_CONFIGURATION;
            setup.wIndex = 0;
            setup.wLength = buf.len() as u16;

            sys::libusb20_dev_request_sync(
                self.dev,
                &mut setup,
                buf.as_mut_ptr() as *mut c_void,
                &mut actual,
                ms,
                0,
            )
        };

        if rc != 0 {
            return Err(UsbError::Control(rc));
        }
        buf.truncate(actual as usize);
        Ok(buf)
    }

    /// Reads the configuration descriptor and finds the MTP interface.
    pub fn find_mtp_interface(&mut self, timeout: Duration) -> Result<MtpInterface, UsbError> {
        let raw = self.config_descriptor_raw(timeout)?;
        let cfg = ConfigDescriptor::parse(&raw)?;
        Ok(MtpInterface::find(&cfg)?)
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
            return Err(UsbError::Open(rc));
        }
        Ok(())
    }

    /// Resets the device.
    ///
    /// A reset clears a session that an earlier program left open. `usbconfig`
    /// needs root for a reset, and this call does not.
    pub fn reset(&mut self) -> Result<(), UsbError> {
        // SAFETY: `self.dev` is open.
        let rc = unsafe { sys::libusb20_dev_reset(self.dev) };
        if rc != 0 {
            return Err(UsbError::Reset(rc));
        }
        Ok(())
    }

    /// Opens one bulk endpoint and gives a channel for transfers.
    ///
    /// `slot` chooses a transfer slot. Each open endpoint needs its own slot,
    /// and `slot` must be below the count given to [`DeviceHandle::open`].
    pub fn open_bulk(
        &mut self,
        slot: u16,
        endpoint: u8,
        buffer_size: u32,
    ) -> Result<BulkChannel<'_>, UsbError> {
        // SAFETY: `self.dev` is open, and the library gives a transfer for the
        // slot or null.
        let xfer = unsafe { sys::libusb20_tr_get_pointer(self.dev, slot) };
        if xfer.is_null() {
            return Err(UsbError::EndpointOpen { endpoint, code: -1 });
        }

        // SAFETY: `xfer` is a valid transfer for this device. One frame is
        // enough, because the channel does one transfer at a time.
        let rc = unsafe { sys::libusb20_tr_open(xfer, buffer_size, 1, endpoint) };
        if rc != 0 {
            return Err(UsbError::EndpointOpen { endpoint, code: rc });
        }

        Ok(BulkChannel {
            xfer,
            endpoint,
            _marker: PhantomData,
        })
    }

    /// Opens the two bulk endpoints of an MTP interface.
    ///
    /// One call gives both channels. A caller cannot borrow the device two
    /// times, so a caller cannot open the two endpoints in two calls.
    pub fn open_mtp(
        &mut self,
        iface: &MtpInterface,
        buffer_size: u32,
    ) -> Result<MtpChannels<'_>, UsbError> {
        // SAFETY: the two calls below use two different slots, so the two
        // transfers do not share state. Each raw pointer stays valid while the
        // device is open, and `MtpChannels` borrows the device.
        let write = {
            let xfer = unsafe { sys::libusb20_tr_get_pointer(self.dev, 0) };
            open_one(xfer, iface.bulk_out, buffer_size)?
        };
        let read = {
            let xfer = unsafe { sys::libusb20_tr_get_pointer(self.dev, 1) };
            open_one(xfer, iface.bulk_in, buffer_size)?
        };
        Ok(MtpChannels { write, read })
    }
}

/// Opens one transfer for an endpoint.
fn open_one<'a>(
    xfer: *mut sys::libusb20_transfer,
    endpoint: u8,
    buffer_size: u32,
) -> Result<BulkChannel<'a>, UsbError> {
    if xfer.is_null() {
        return Err(UsbError::EndpointOpen { endpoint, code: -1 });
    }
    // SAFETY: `xfer` is a valid transfer that the open device owns.
    let rc = unsafe { sys::libusb20_tr_open(xfer, buffer_size, 1, endpoint) };
    if rc != 0 {
        return Err(UsbError::EndpointOpen { endpoint, code: rc });
    }
    Ok(BulkChannel {
        xfer,
        endpoint,
        _marker: PhantomData,
    })
}

/// The two bulk channels of an MTP interface.
pub struct MtpChannels<'a> {
    /// The channel that sends data to the device.
    pub write: BulkChannel<'a>,
    /// The channel that reads data from the device.
    pub read: BulkChannel<'a>,
}

impl Drop for OpenDevice<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.dev` is open, and this code runs one time.
        unsafe { sys::libusb20_dev_close(self.dev) };
    }
}

/// One bulk endpoint, open for transfers.
pub struct BulkChannel<'a> {
    xfer: *mut sys::libusb20_transfer,
    endpoint: u8,
    _marker: PhantomData<&'a mut ()>,
}

impl BulkChannel<'_> {
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

impl Drop for BulkChannel<'_> {
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
