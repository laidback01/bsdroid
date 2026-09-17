//! Support code that loads a capture fixture.
//!
//! The parser lives in the `hex-fixture` crate, because `ptp-proto` and
//! `usb-freebsd` both keep fixtures and an earlier version held a copy of the
//! parser in each one.

/// Reads a fixture from `tests/fixtures` of this crate.
pub fn load(name: &str) -> Vec<u8> {
    hex_fixture::load(env!("CARGO_MANIFEST_DIR"), name)
}
