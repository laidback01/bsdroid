//! USB transport for FreeBSD, over `libusb20`.
//!
//! FreeBSD has no native `libusb-1.0`. The file `/usr/lib/libusb.so.3` holds a
//! compatibility layer, and that layer enters an endless loop when an MTP
//! transfer stalls. `docs/00-why.md` records the measurement.
//!
//! This crate calls `libusb20`, which is the native FreeBSD USB library.
//! `libusb20` gives a synchronous bulk transfer with a timeout the caller
//! controls:
//!
//! ```text
//! libusb20_tr_bulk_intr_sync(xfer, buf, len, &actlen, timeout_ms)
//! ```
//!
//! One call does one transfer. There is no event loop, so there is no loop to
//! get stuck in.

pub mod descriptor;

#[allow(clippy::all)]
mod sys;
