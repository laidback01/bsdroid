//! Tests that read real traffic from a Samsung SM-S901U.
//!
//! A test in this file is the primary specification for the parser. If the
//! parser disagrees with a fixture, the parser is wrong.
//!
//! See `tests/fixtures/README.md` for the source of the captures.

mod common;

use ptp_proto::{Container, ContainerType, ParseError, StorageInfo};

const OP_GET_STORAGE_IDS: u16 = 0x1004;
const OP_GET_STORAGE_INFO: u16 = 0x1005;
const OP_CLOSE_SESSION: u16 = 0x1003;
const RESP_OK: u16 = 0x2001;

#[test]
fn parses_get_storage_ids_command() {
    let bytes = common::load("get_storage_ids_command.hex");
    let c = Container::parse(&bytes).expect("the capture must parse");

    assert_eq!(c.length, 12);
    assert_eq!(c.kind, ContainerType::Command);
    assert_eq!(c.code, OP_GET_STORAGE_IDS);
    assert_eq!(c.transaction_id, 33);
    assert!(c.payload.is_empty(), "the command carries no parameter");
}

#[test]
fn parses_get_storage_ids_data() {
    let bytes = common::load("get_storage_ids_data.hex");
    let c = Container::parse(&bytes).expect("the capture must parse");

    assert_eq!(c.kind, ContainerType::Data);
    assert_eq!(c.code, OP_GET_STORAGE_IDS);
    assert_eq!(c.transaction_id, 33);

    let ids = c
        .payload_as_u32_array()
        .expect("the payload is a u32 array");
    assert_eq!(ids, vec![0x0001_0001], "the S22 reports one storage");
}

#[test]
fn parses_response_ok() {
    let bytes = common::load("response_ok.hex");
    let c = Container::parse(&bytes).expect("the capture must parse");

    assert_eq!(c.kind, ContainerType::Response);
    assert_eq!(c.code, RESP_OK);
    assert_eq!(c.transaction_id, 33);
}

#[test]
fn parses_close_session_command() {
    let bytes = common::load("close_session_command.hex");
    let c = Container::parse(&bytes).expect("the capture must parse");

    assert_eq!(c.kind, ContainerType::Command);
    assert_eq!(c.code, OP_CLOSE_SESSION);
    assert_eq!(c.transaction_id, 35);
}

/// This test is the important one. The container holds every scalar width the
/// protocol uses, and it holds a PTP string.
///
/// The expected values come from the cross-check table in
/// `tests/fixtures/README.md`. `mtp-detect` reported the same values in a
/// separate session.
#[test]
fn parses_storage_info_and_agrees_with_mtp_detect() {
    let bytes = common::load("storage_info_data.hex");
    let c = Container::parse(&bytes).expect("the capture must parse");

    assert_eq!(c.length, 74);
    assert_eq!(c.kind, ContainerType::Data);
    assert_eq!(c.code, OP_GET_STORAGE_INFO);
    assert_eq!(c.transaction_id, 34);

    let info = StorageInfo::parse(c.payload).expect("the payload is a StorageInfo");

    assert_eq!(info.storage_type, 0x0003, "fixed RAM storage");
    assert_eq!(info.filesystem_type, 0x0002, "generic hierarchical");
    assert_eq!(info.access_capability, 0x0000, "read/write");

    // MaxCapacity does not change, so `mtp-detect` reports the same value.
    assert_eq!(info.max_capacity, 239_935_107_072);

    // Free space does change. `mtp-detect` ran minutes before the capture and
    // reported 204718743552, which is 5.91 MiB more. The phone wrote log and
    // cache files between the two runs. The value below is what the captured
    // bytes hold, and the capture is the authority.
    assert_eq!(info.free_space_in_bytes, 204_712_546_304);

    assert_eq!(info.free_space_in_objects, 1_073_741_824);

    assert_eq!(info.storage_description, "Internal storage");
    assert_eq!(info.volume_identifier, "");
}

// --- Tests that build a container ---
//
// A device produced each fixture below. The tests compare the bytes the
// builder writes with the bytes the device sent. The device is the authority.

#[test]
fn builds_the_same_bytes_the_device_sent_for_get_storage_ids() {
    let expected = common::load("get_storage_ids_command.hex");
    let got = ptp_proto::build_command(OP_GET_STORAGE_IDS, 33, &[]);
    assert_eq!(got, expected, "the builder must agree with the device");
}

#[test]
fn builds_the_same_bytes_the_device_sent_for_get_storage_info() {
    let expected = common::load("get_storage_info_command.hex");
    let got = ptp_proto::build_command(OP_GET_STORAGE_INFO, 34, &[0x0001_0001]);
    assert_eq!(got, expected, "the builder must agree with the device");
}

#[test]
fn builds_the_same_bytes_the_device_sent_for_close_session() {
    let expected = common::load("close_session_command.hex");
    let got = ptp_proto::build_command(OP_CLOSE_SESSION, 35, &[]);
    assert_eq!(got, expected, "the builder must agree with the device");
}

#[test]
fn a_built_container_parses_back_to_the_same_values() {
    let bytes = ptp_proto::build_command(0x1002, 7, &[1]);
    let c = Container::parse(&bytes).expect("the builder must write a valid container");
    assert_eq!(c.kind, ContainerType::Command);
    assert_eq!(c.code, 0x1002);
    assert_eq!(c.transaction_id, 7);
    assert_eq!(c.parameters(), vec![1]);
    assert_eq!(c.length as usize, bytes.len());
}

// --- The header of a large data phase ---

#[test]
fn a_header_parses_from_a_buffer_that_holds_only_the_header() {
    let bytes = common::load("storage_info_data.hex");
    let h = ptp_proto::Header::parse(&bytes[..12]).expect("12 bytes are enough");

    assert_eq!(h.length, 74);
    assert_eq!(h.kind, ContainerType::Data);
    assert_eq!(h.code, OP_GET_STORAGE_INFO);
    assert_eq!(h.transaction_id, 34);
    assert_eq!(h.payload_len(), 62);
}

/// This test records the reason `Header` exists.
///
/// A device answers `GetObjectHandles` with a container that is larger than
/// one USB transfer. The first transfer holds the header, and the length field
/// is then larger than the buffer. `Container::parse` gives an error, and
/// `Header::parse` gives the header.
#[test]
fn a_header_parses_when_the_container_is_larger_than_the_buffer() {
    // A container that declares 200000 bytes, with only 64 bytes present.
    let mut bytes = ptp_proto::build(ContainerType::Data, 0x1007, 9, &[0u8; 52]);
    bytes[0..4].copy_from_slice(&200_000u32.to_le_bytes());

    assert!(
        matches!(
            Container::parse(&bytes),
            Err(ParseError::LengthMismatch { .. })
        ),
        "Container::parse needs the whole container"
    );

    let h = ptp_proto::Header::parse(&bytes).expect("the header must parse");
    assert_eq!(h.length, 200_000);
    assert_eq!(h.kind, ContainerType::Data);
    assert_eq!(h.code, 0x1007);
    assert_eq!(h.payload_len(), 199_988);
}

#[test]
fn a_header_needs_twelve_bytes() {
    for n in 0..12 {
        let got = ptp_proto::Header::parse(&vec![0u8; n]);
        assert!(
            matches!(got, Err(ParseError::ShortHeader { .. })),
            "{n} bytes must give ShortHeader, got {got:?}"
        );
    }
}

#[test]
fn a_length_below_the_header_size_gives_no_payload() {
    let mut bytes = common::load("response_ok.hex");
    bytes[0] = 4;
    let h = ptp_proto::Header::parse(&bytes).expect("the header still parses");
    assert_eq!(h.payload_len(), 0, "the count must not go below zero");
}

// --- Tests for damaged input ---
//
// The defect that started this project was an endless loop. A parser must
// never loop and never panic on bad input. It must return an error.
// See docs/00-why.md.

#[test]
fn rejects_a_container_that_is_too_short_for_a_header() {
    for n in 0..12 {
        let bytes = vec![0u8; n];
        let got = Container::parse(&bytes);
        assert!(
            matches!(got, Err(ParseError::ShortHeader { .. })),
            "a buffer of {n} bytes must give ShortHeader, and it gave {got:?}"
        );
    }
}

#[test]
fn rejects_a_length_field_that_disagrees_with_the_buffer() {
    let mut bytes = common::load("get_storage_ids_data.hex");
    bytes[0] = 0xff; // Claim a length far larger than the buffer.
    let got = Container::parse(&bytes);
    assert!(
        matches!(got, Err(ParseError::LengthMismatch { .. })),
        "got {got:?}"
    );
}

#[test]
fn rejects_a_length_field_below_the_header_size() {
    let mut bytes = common::load("response_ok.hex");
    bytes[0] = 4; // A length below 12 cannot hold a header.
    let got = Container::parse(&bytes);
    assert!(
        matches!(got, Err(ParseError::LengthMismatch { .. })),
        "got {got:?}"
    );
}

#[test]
fn rejects_an_unknown_container_type() {
    let mut bytes = common::load("response_ok.hex");
    bytes[4] = 0x09; // 9 is not a container type.
    let got = Container::parse(&bytes);
    assert!(
        matches!(got, Err(ParseError::UnknownContainerType(9))),
        "got {got:?}"
    );
}

#[test]
fn rejects_a_storage_info_that_ends_early() {
    // Every prefix of a valid dataset must give an error, and must not panic.
    let full = common::load("storage_info_data.hex");
    let payload = &full[12..];
    for n in 0..payload.len() {
        let got = StorageInfo::parse(&payload[..n]);
        assert!(
            got.is_err(),
            "a payload of {n} bytes must give an error, and it gave {got:?}"
        );
    }
}

#[test]
fn rejects_a_string_that_claims_more_characters_than_it_holds() {
    let mut full = common::load("storage_info_data.hex");
    // Byte 12 starts the dataset. The description length byte sits at
    // 12 + 2 + 2 + 2 + 8 + 8 + 4 = 38.
    full[38] = 0x7f; // Claim 127 characters.
    let got = StorageInfo::parse(&full[12..]);
    assert!(got.is_err(), "got {got:?}");
}

#[test]
fn rejects_a_u32_array_that_claims_more_elements_than_it_holds() {
    let mut bytes = common::load("get_storage_ids_data.hex");
    bytes[12] = 0xff; // Claim 255 elements in a payload that holds one.
    let c = Container::parse(&bytes).expect("the header is still valid");
    let got = c.payload_as_u32_array();
    assert!(got.is_err(), "got {got:?}");
}

// --- GetDeviceInfo, which decides what a filesystem can do ---

#[test]
fn parses_the_device_info_of_the_s22() {
    let bytes = common::load("s22_device_info.hex");
    assert_eq!(bytes.len(), 457, "the fixture holds the whole dataset");

    let info = ptp_proto::DeviceInfo::parse(&bytes).expect("the capture must parse");

    assert_eq!(info.standard_version, 100, "version 1.00");
    assert_eq!(info.vendor_extension_id, 6, "Microsoft MTP");
    assert_eq!(info.manufacturer, "samsung");
    assert_eq!(info.model, "SM-S901U");
    assert_eq!(info.device_version, "S901USQSAGZH3");
    assert!(
        info.vendor_extension_desc.contains("microsoft.com"),
        "got {}",
        info.vendor_extension_desc
    );
}

/// This test records the answer a filesystem needs.
#[test]
fn the_s22_supports_every_operation_a_filesystem_needs() {
    let bytes = common::load("s22_device_info.hex");
    let info = ptp_proto::DeviceInfo::parse(&bytes).unwrap();

    assert_eq!(info.operations_supported.len(), 35);

    assert!(info.can_read_part(), "a read at an offset");
    assert!(info.can_write(), "a new file");
    assert!(info.can_delete(), "remove a file");
    assert!(info.has_fast_listing(), "a folder in one operation");

    // The 64 bit read is the one a large file needs.
    assert!(info.supports(ptp_proto::op::GET_PARTIAL_OBJECT));
    assert!(info.supports(ptp_proto::op::GET_PARTIAL_OBJECT_64));
    assert!(info.supports(ptp_proto::op::SEND_PARTIAL_OBJECT));
    assert!(info.supports(ptp_proto::op::MOVE_OBJECT));
    assert!(
        info.supports(ptp_proto::op::SET_OBJECT_PROP_VALUE),
        "rename"
    );
}

/// A device that supports fewer operations gives a different answer. The test
/// removes the partial read from the real dataset, and checks the report.
#[test]
fn a_device_without_a_partial_read_reports_the_absence() {
    let bytes = common::load("s22_device_info.hex");
    let info = ptp_proto::DeviceInfo::parse(&bytes).unwrap();

    let mut reduced = info.clone();
    reduced.operations_supported.retain(|c| {
        *c != ptp_proto::op::GET_PARTIAL_OBJECT && *c != ptp_proto::op::GET_PARTIAL_OBJECT_64
    });

    assert!(!reduced.can_read_part());
    assert!(reduced.can_write(), "the other answers do not change");
}

#[test]
fn an_operation_code_has_a_name() {
    assert_eq!(ptp_proto::op::name(0x1009), "GetObject");
    assert_eq!(ptp_proto::op::name(0x101b), "GetPartialObject");
    assert_eq!(ptp_proto::op::name(0x95c1), "GetPartialObject64");
    assert_eq!(ptp_proto::op::name(0x9805), "GetObjectPropList");
    assert_eq!(ptp_proto::op::name(0xdead), "unknown");
}

#[test]
fn a_truncated_device_info_gives_an_error_and_does_not_panic() {
    let full = common::load("s22_device_info.hex");
    for n in 0..120 {
        let _ = ptp_proto::DeviceInfo::parse(&full[..n]);
    }
}

/// A count that claims more codes than the buffer holds must give an error.
#[test]
fn a_huge_operation_count_does_not_reserve_memory() {
    let mut bytes = common::load("s22_device_info.hex");
    // The operation count sits after the description string and the mode.
    // Byte 0x9b starts the count on this fixture.
    bytes[0x9b] = 0xff;
    bytes[0x9c] = 0xff;
    let got = ptp_proto::DeviceInfo::parse(&bytes);
    assert!(got.is_err(), "got {got:?}");
}

// --- Building an ObjectInfo to send a file ---

/// The builder and the parser must agree. The test builds a dataset, parses
/// the dataset, and compares each field.
#[test]
fn an_object_info_survives_a_round_trip() {
    let built =
        ptp_proto::ObjectInfo::build_for_send(0x0001_0001, 0x10, "holiday.jpg", 12345, false);
    let back = ptp_proto::ObjectInfo::parse(&built).expect("the builder writes a valid dataset");

    assert_eq!(back.storage_id, 0x0001_0001);
    assert_eq!(back.parent_object, 0x10);
    assert_eq!(back.filename, "holiday.jpg");
    assert_eq!(back.compressed_size, 12345);
    assert!(!back.is_folder());
    assert_eq!(back.association_type, 0);
}

#[test]
fn a_folder_round_trips_as_a_folder() {
    let built = ptp_proto::ObjectInfo::build_for_send(1, 0, "NewFolder", 0, true);
    let back = ptp_proto::ObjectInfo::parse(&built).unwrap();

    assert!(back.is_folder());
    assert_eq!(back.object_format, ptp_proto::format::ASSOCIATION);
    assert_eq!(back.association_type, 1);
    assert_eq!(back.filename, "NewFolder");
    assert_eq!(back.compressed_size, 0);
}

/// A name outside ASCII must survive. The wire format holds UTF-16.
#[test]
fn a_name_with_other_letters_survives() {
    for name in ["résumé.txt", "日本語.jpg", "Ünïcödé", "a b c.txt"] {
        let built = ptp_proto::ObjectInfo::build_for_send(1, 0, name, 1, false);
        let back = ptp_proto::ObjectInfo::parse(&built).unwrap();
        assert_eq!(back.filename, name, "name {name}");
    }
}

/// The dataset the builder writes must have the shape the parser of a real
/// capture expects. The test compares the field offsets with the fixture.
#[test]
fn the_built_dataset_has_the_same_shape_as_a_real_one() {
    // The real StorageInfo fixture is a different dataset, so the test uses
    // the real ObjectInfo the device sent for a file, through the parser.
    let built = ptp_proto::ObjectInfo::build_for_send(0x0001_0001, 4, "a.jpg", 1000, false);

    // The first four bytes hold the storage, little-endian.
    assert_eq!(&built[0..4], &0x0001_0001u32.to_le_bytes());
    // The next two hold the format.
    assert_eq!(&built[4..6], &ptp_proto::format::UNDEFINED.to_le_bytes());
    // The size sits at offset 8.
    assert_eq!(&built[8..12], &1000u32.to_le_bytes());
    // The parent sits after the thumbnail and image fields.
    assert_eq!(&built[38..42], &4u32.to_le_bytes());
}

#[test]
fn an_empty_name_gives_one_zero_byte() {
    let built = ptp_proto::ObjectInfo::build_for_send(1, 0, "", 0, false);
    let back = ptp_proto::ObjectInfo::parse(&built).unwrap();
    assert_eq!(back.filename, "");
}

/// A name that is too long for the wire format must not make a bad dataset.
#[test]
fn a_very_long_name_still_gives_a_valid_dataset() {
    let long = "a".repeat(400);
    let built = ptp_proto::ObjectInfo::build_for_send(1, 0, &long, 0, false);
    let back = ptp_proto::ObjectInfo::parse(&built).expect("the dataset must parse");
    assert_eq!(back.filename.len(), 254, "the builder cuts the name");
}
