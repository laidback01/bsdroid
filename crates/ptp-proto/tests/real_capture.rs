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
