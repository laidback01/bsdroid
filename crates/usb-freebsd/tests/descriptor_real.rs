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

/// The adb interface tells the host that the device runs Android.
///
/// A device that shows adb and shows no MTP gets a better message than a
/// generic "no MTP device". The adb interface is the sign of Android.
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

// --- Charge mode ---
//
// The same phone in charge mode gives a different descriptor. The tests below
// record what a host can learn from the descriptor, and what a host cannot.

#[test]
fn the_charge_mode_fixture_agrees_with_its_own_header() {
    let bytes = common::load("s22_charge_mode_descriptor.hex");
    assert_eq!(bytes.len(), 70);
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");
    assert_eq!(cfg.total_length, 70);
    assert_eq!(cfg.num_interfaces, 2);
}

/// This test records the finding that matters for a diagnostic message.
///
/// A phone in charge mode still gives the MTP interface. A host that finds an
/// MTP interface does not know that the host can read a file.
#[test]
fn charge_mode_still_gives_an_mtp_interface() {
    let bytes = common::load("s22_charge_mode_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let mtp = MtpInterface::find(&cfg).expect("charge mode still gives MTP");
    assert_eq!(mtp.interface_number, 0);
    assert_eq!(mtp.bulk_in, 0x81);
    assert_eq!(mtp.bulk_out, 0x01);
    assert_eq!(mtp.interrupt_in, Some(0x82));
}

/// The MTP interface is the same in both captures. A host cannot compare the
/// two interfaces and learn anything about file access.
#[test]
fn the_mtp_interface_is_the_same_in_both_captures() {
    let transfer = common::load("s22_config_descriptor.hex");
    let charge = common::load("s22_charge_mode_descriptor.hex");

    let a = MtpInterface::find(&ConfigDescriptor::parse(&transfer).unwrap()).unwrap();
    let b = MtpInterface::find(&ConfigDescriptor::parse(&charge).unwrap()).unwrap();

    assert_eq!(a.interface_number, b.interface_number);
    assert_eq!(a.bulk_in, b.bulk_in);
    assert_eq!(a.bulk_out, b.bulk_out);
    assert_eq!(a.interrupt_in, b.interrupt_in);
    assert_eq!(a.max_packet_size, b.max_packet_size);
}

/// The adb interface number is not the same in the two captures. A host must
/// not remember an interface number from an earlier connect.
///
/// The test does not claim that the USB mode sets the number. A later capture
/// of the same phone, in the same mode, gave the other layout. See
/// `docs/02-device-states.md`.
#[test]
fn the_adb_interface_number_is_not_stable_across_captures() {
    let transfer = common::load("s22_config_descriptor.hex");
    let charge = common::load("s22_charge_mode_descriptor.hex");

    let adb_number = |bytes: &[u8]| -> u8 {
        ConfigDescriptor::parse(bytes)
            .unwrap()
            .interfaces
            .iter()
            .find(|i| i.is_adb())
            .expect("both modes give adb")
            .number
    };

    assert_eq!(adb_number(&transfer), 3, "capture A, file transfer");
    assert_eq!(adb_number(&charge), 1, "capture B, charge and locked");
}

// --- Image transfer mode, which carries PTP ---

#[test]
fn the_ptp_fixture_agrees_with_its_own_header() {
    let bytes = common::load("s22_ptp_mode_descriptor.hex");
    assert_eq!(bytes.len(), 70);
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");
    assert_eq!(cfg.total_length, 70);
    assert_eq!(cfg.num_interfaces, 2);
}

/// Image transfer mode gives file access, and the interface looks like MTP.
///
/// MTP is an extension of PTP, so the two share the interface class. The host
/// opens the same endpoints, and the host reads the storage.
#[test]
fn image_mode_gives_an_interface_that_matches_the_mtp_class() {
    let bytes = common::load("s22_ptp_mode_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let mtp = MtpInterface::find(&cfg).expect("image mode gives a still imaging interface");
    assert_eq!(mtp.interface_number, 0);
    assert_eq!(mtp.bulk_in, 0x81);
    assert_eq!(mtp.bulk_out, 0x01);
    assert_eq!(mtp.interrupt_in, Some(0x82));
    assert_eq!(mtp.max_packet_size, 512);
}

/// The class fields do not separate the two modes.
///
/// A host that reads only the class, the subclass and the protocol cannot tell
/// file transfer mode from image transfer mode.
#[test]
fn the_class_fields_are_the_same_in_file_transfer_and_image_modes() {
    let transfer = common::load("s22_config_descriptor.hex");
    let image = common::load("s22_ptp_mode_descriptor.hex");

    let first = |bytes: &[u8]| -> (u8, u8, u8) {
        let cfg = ConfigDescriptor::parse(bytes).unwrap();
        let i = &cfg.interfaces[0];
        (i.class, i.subclass, i.protocol)
    };

    assert_eq!(first(&transfer), (0x06, 0x01, 0x01));
    assert_eq!(first(&image), (0x06, 0x01, 0x01));
    assert_eq!(first(&transfer), first(&image));
}

/// The interface string index does separate the two modes.
///
/// The field is the one difference the configuration descriptor holds. The
/// device descriptor also differs, and `idProduct` is 0x6860 for file transfer
/// and 0x6866 for image transfer.
#[test]
fn the_interface_string_index_separates_file_transfer_from_image_mode() {
    let transfer = common::load("s22_config_descriptor.hex");
    let image = common::load("s22_ptp_mode_descriptor.hex");

    let index =
        |bytes: &[u8]| -> u8 { ConfigDescriptor::parse(bytes).unwrap().interfaces[0].string_index };

    assert_eq!(index(&transfer), 5, "file transfer names the interface");
    assert_eq!(index(&image), 0, "image transfer gives no name");
    assert_ne!(index(&transfer), index(&image));
}

// --- USB tethering mode ---

#[test]
fn the_tethering_fixture_agrees_with_its_own_header() {
    let bytes = common::load("s22_tethering_descriptor.hex");
    assert_eq!(bytes.len(), 106);
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");
    assert_eq!(cfg.total_length, 106);
    assert_eq!(cfg.num_interfaces, 3);
}

/// Tethering mode gives no MTP interface. The mode is different from charge
/// mode, which keeps the interface.
#[test]
fn tethering_gives_no_mtp_interface() {
    let bytes = common::load("s22_tethering_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    assert_eq!(
        MtpInterface::find(&cfg),
        Err(DescriptorError::NoMtpInterface)
    );
    assert!(cfg.interfaces.iter().all(|i| !i.is_mtp()));
}

/// The host still knows that the device runs Android.
#[test]
fn tethering_still_shows_the_adb_interface() {
    let bytes = common::load("s22_tethering_descriptor.hex");
    let cfg = ConfigDescriptor::parse(&bytes).expect("the capture must parse");

    let adb: Vec<u8> = cfg
        .interfaces
        .iter()
        .filter(|i| i.is_adb())
        .map(|i| i.number)
        .collect();
    assert_eq!(adb, vec![2], "interface 2 carries adb in this mode");
}

/// This test is the reason the parser checks the interface class first.
///
/// In tethering mode, the CDC data interface owns endpoint 0x81 and endpoint
/// 0x01. The same two addresses carry MTP in file transfer mode and in image
/// mode. A parser that chooses the first bulk pair opens the network
/// interface, and then sends PTP to a network device.
#[test]
fn the_network_interface_owns_the_addresses_that_mtp_uses_elsewhere() {
    let tethering = common::load("s22_tethering_descriptor.hex");
    let transfer = common::load("s22_config_descriptor.hex");

    // In file transfer mode the addresses belong to MTP.
    let mtp = MtpInterface::find(&ConfigDescriptor::parse(&transfer).unwrap()).unwrap();
    assert_eq!((mtp.bulk_in, mtp.bulk_out), (0x81, 0x01));

    // In tethering mode the same addresses belong to CDC data, which is
    // class 0x0a. The parser gives no MTP interface, so no caller can reach
    // the addresses by accident.
    let cfg = ConfigDescriptor::parse(&tethering).unwrap();
    let owner = cfg
        .interfaces
        .iter()
        .find(|i| i.endpoints.iter().any(|e| e.address == 0x81))
        .expect("some interface owns 0x81");
    assert_eq!(owner.class, 0x0a, "CDC data owns the address here");
    assert!(!owner.is_mtp());
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
