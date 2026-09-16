//! Tests that read a real USB configuration descriptor.
//!
//! The descriptor comes from a Samsung SM-S901U with USB debugging on. The
//! phone shows four interfaces, and three of them are not MTP. Two of the
//! three have bulk endpoints.
//!
//! See `tests/fixtures/s22_config_descriptor.hex`.

mod common;

use usb_freebsd::descriptor::{ConfigDescriptor, DescriptorError, MtpInterface};

/// The MTP interface class, subclass and protocol. The USB standard calls the
/// class "still imaging".
const MTP_CLASS: u8 = 0x06;
const MTP_SUBCLASS: u8 = 0x01;
const MTP_PROTOCOL: u8 = 0x01;

#[test]
fn reads_the_configuration_header() {
    let bytes = common::load("s22_config_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    assert_eq!(cfg.total_length, 136);
    assert_eq!(cfg.num_interfaces, 4);
    assert_eq!(cfg.configuration_value, 1);
}

#[test]
fn the_fixture_length_agrees_with_the_header() {
    // A descriptor that disagrees with its own length field is a broken
    // capture. This test guards the fixture, not the parser.
    let bytes = common::load("s22_config_descriptor.hex");
    assert_eq!(bytes.len(), 136);
}

#[test]
fn finds_all_four_interfaces() {
    let bytes = common::load("s22_config_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let got: Vec<(u8, u8, u8, u8)> = cfg
        .interfaces
        .iter()
        .map(|i| (i.number, i.class, i.subclass, i.protocol))
        .collect();

    assert_eq!(
        got,
        vec![
            (0, 0x06, 0x01, 0x01), // MTP
            (1, 0x02, 0x02, 0x01), // CDC control
            (2, 0x0a, 0x00, 0x00), // CDC data
            (3, 0xff, 0x42, 0x01), // adb
        ]
    );
}

#[test]
fn assigns_each_endpoint_to_the_interface_that_owns_it() {
    let bytes = common::load("s22_config_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let addresses: Vec<Vec<u8>> = cfg
        .interfaces
        .iter()
        .map(|i| i.endpoints.iter().map(|e| e.address).collect())
        .collect();

    assert_eq!(
        addresses,
        vec![
            vec![0x81, 0x01, 0x82], // MTP
            vec![0x84],             // CDC control
            vec![0x83, 0x02],       // CDC data
            vec![0x03, 0x85],       // adb
        ],
        "a class descriptor between the endpoints must not move an endpoint"
    );
}

/// This is the test that matters. The parser must choose interface 0.
#[test]
fn finds_the_mtp_interface_and_not_the_adb_interface() {
    let bytes = common::load("s22_config_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let mtp = MtpInterface::find(&cfg).expect("the phone gives an MTP interface");

    assert_eq!(mtp.interface_number, 0);
    assert_eq!(mtp.class, MTP_CLASS);
    assert_eq!(mtp.subclass, MTP_SUBCLASS);
    assert_eq!(mtp.protocol, MTP_PROTOCOL);

    // The direction bit is 0x80. Address 0x81 sends data to the host.
    assert_eq!(mtp.bulk_in, 0x81);
    assert_eq!(mtp.bulk_out, 0x01);
    assert_eq!(mtp.interrupt_in, Some(0x82));

    assert_eq!(mtp.max_packet_size, 512);
}

#[test]
fn does_not_choose_a_bulk_endpoint_from_another_interface() {
    let bytes = common::load("s22_config_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");
    let mtp = MtpInterface::find(&cfg).expect("the phone gives an MTP interface");

    // These addresses belong to the CDC data interface and the adb interface.
    for wrong in [0x83, 0x02, 0x03, 0x85, 0x84] {
        assert_ne!(mtp.bulk_in, wrong, "address {wrong:#04x} is not MTP");
        assert_ne!(mtp.bulk_out, wrong, "address {wrong:#04x} is not MTP");
    }
}

/// A device in charge-only mode shows no MTP interface. If the device still
/// shows adb, the host knows the device runs Android, and the host can give a
/// better message than "no MTP device".
#[test]
fn finds_the_adb_interface_and_does_not_confuse_it_with_mtp() {
    let bytes = common::load("s22_config_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let adb: Vec<u8> = cfg
        .interfaces
        .iter()
        .filter(|i| i.is_adb())
        .map(|i| i.number)
        .collect();
    assert_eq!(adb, vec![3], "interface 3 carries adb");

    let mtp: Vec<u8> = cfg
        .interfaces
        .iter()
        .filter(|i| i.is_mtp())
        .map(|i| i.number)
        .collect();
    assert_eq!(mtp, vec![0], "interface 0 carries MTP");

    // No interface is both.
    assert!(cfg.interfaces.iter().all(|i| !(i.is_mtp() && i.is_adb())));
}

#[test]
fn reports_absence_when_the_device_has_no_mtp_interface() {
    // Take the real descriptor and change the class of interface 0. The phone
    // then looks like a device in charge-only mode.
    let mut bytes = common::load("s22_config_descriptor.hex");
    bytes[9 + 5] = 0xff; // bInterfaceClass of interface 0

    let cfg = ConfigDescriptor::parse(&bytes).expect("the header is still valid");
    assert_eq!(
        MtpInterface::find(&cfg),
        Err(DescriptorError::NoMtpInterface)
    );
}

// --- Tests for damaged input ---
//
// Rule 3 in docs/00-why.md: a parse function never panics.

#[test]
fn rejects_a_buffer_that_is_too_short() {
    for n in 0..9 {
        let bytes = vec![0u8; n];
        let got = ConfigDescriptor::parse(&bytes);
        assert!(got.is_err(), "{n} bytes must give an error, got {got:?}");
    }
}

#[test]
fn rejects_a_descriptor_with_the_wrong_type() {
    let mut bytes = common::load("s22_config_descriptor.hex");
    bytes[1] = 0x01; // 0x01 is DEVICE, not CONFIGURATION
    let got = ConfigDescriptor::parse(&bytes);
    assert!(
        matches!(got, Err(DescriptorError::WrongType { .. })),
        "got {got:?}"
    );
}

#[test]
fn survives_every_truncation_of_a_real_descriptor() {
    // No prefix may panic and no prefix may loop.
    let full = common::load("s22_config_descriptor.hex");
    for n in 0..full.len() {
        let _ = ConfigDescriptor::parse(&full[..n]);
    }
}

/// A descriptor with `bLength` of 0 is the classic endless-loop trap. A parser
/// that advances by `bLength` never advances, so the parser never stops.
#[test]
fn a_zero_length_descriptor_does_not_cause_an_endless_loop() {
    let mut bytes = common::load("s22_config_descriptor.hex");
    bytes[9] = 0x00; // bLength of the first interface descriptor

    let got = ConfigDescriptor::parse(&bytes);
    assert!(
        matches!(got, Err(DescriptorError::ZeroLength { .. })),
        "got {got:?}"
    );
}

#[test]
fn a_descriptor_that_runs_past_the_buffer_is_an_error() {
    let mut bytes = common::load("s22_config_descriptor.hex");
    bytes[9] = 0xf0; // The interface descriptor claims 240 bytes.
    let got = ConfigDescriptor::parse(&bytes);
    assert!(got.is_err(), "got {got:?}");
}

#[test]
fn an_endpoint_before_any_interface_is_an_error() {
    // Build a configuration header, then an endpoint with no interface.
    let mut bytes = vec![0x09, 0x02, 0x10, 0x00, 0x01, 0x01, 0x00, 0x80, 0xfa];
    bytes.extend_from_slice(&[0x07, 0x05, 0x81, 0x02, 0x00, 0x02, 0x00]);
    let got = ConfigDescriptor::parse(&bytes);
    assert!(
        matches!(got, Err(DescriptorError::EndpointWithoutInterface)),
        "got {got:?}"
    );
}
