//! Reads a capture fixture.
//!
//! A fixture is a text file of annotated hexadecimal bytes. The format keeps
//! the captured bytes readable in a diff, and it lets a comment record the
//! meaning of each field:
//!
//! ```text
//! 0c 00 00 00        # length, 12 bytes
//! 01 00              # container type, 1 means command
//! 04 10              # code, GetStorageIDs
//! 21 00 00 00        # transaction id, 33
//! ```
//!
//! `ptp-proto` and `usb-freebsd` both keep fixtures. An earlier version of
//! this project held a copy of this parser in each one, and the two copies
//! were the same file except for the doc comments.

use std::path::PathBuf;

/// Reads a fixture file and gives the bytes.
///
/// `manifest_dir` is the crate that holds the fixture, which a caller gives
/// as `env!("CARGO_MANIFEST_DIR")`. The file lives in `tests/fixtures` under
/// that folder.
///
/// The function removes a comment, which starts at `#` and ends at the end of
/// the line. The function then removes all whitespace, and reads what remains
/// as pairs of hexadecimal digits.
///
/// # Panics
///
/// The function panics if the file is absent, if a character is not a
/// hexadecimal digit, or if the count of digits is odd. A panic is correct
/// here, because a broken fixture is a broken test.
pub fn load(manifest_dir: &str, name: &str) -> Vec<u8> {
    let mut path = PathBuf::from(manifest_dir);
    path.push("tests/fixtures");
    path.push(name);

    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));

    let mut digits = String::new();
    for line in text.lines() {
        let code = match line.split_once('#') {
            Some((before, _comment)) => before,
            None => line,
        };
        for c in code.chars() {
            if c.is_whitespace() {
                continue;
            }
            assert!(
                c.is_ascii_hexdigit(),
                "fixture {name} holds the character {c:?}, which is not a hexadecimal digit"
            );
            digits.push(c);
        }
    }

    assert!(
        digits.len() % 2 == 0,
        "fixture {name} holds {} hexadecimal digits, which is an odd count",
        digits.len()
    );

    let mut bytes = Vec::with_capacity(digits.len() / 2);
    let raw = digits.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        let hi = hex_value(raw[i]);
        let lo = hex_value(raw[i + 1]);
        bytes.push((hi << 4) | lo);
        i += 2;
    }
    bytes
}

/// Gives the value of one hexadecimal digit.
///
/// The caller checked every character with `is_ascii_hexdigit`, so no other
/// byte reaches this function.
fn hex_value(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        other => unreachable!("the caller checked the digit, and got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::hex_value;

    #[test]
    fn a_digit_reads_in_either_case() {
        assert_eq!(hex_value(b'0'), 0);
        assert_eq!(hex_value(b'9'), 9);
        assert_eq!(hex_value(b'a'), 10);
        assert_eq!(hex_value(b'F'), 15);
    }
}
