//! A filesystem for an Android device, over MTP.
//!
//! The crate holds the parts of a filesystem that do no I/O. A test runs the
//! parts with no device.
//!
//! See `docs/07-filesystem-design.md`.

pub mod backend;
pub mod tree;
