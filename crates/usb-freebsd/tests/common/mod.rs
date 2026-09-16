//! Support code that loads a capture fixture.
//!
//! The format matches the format the `ptp-proto` crate uses. A comment starts
//! at `#`. The parser removes whitespace and reads hexadecimal bytes.

use std::path::PathBuf;

/// Reads a fixture file and returns the bytes.
///
/// The function panics if the file is absent or the content is not valid. A
/// panic is correct here, because a broken fixture is a broken test.
pub fn load(name: &str) -> Vec<u8> {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
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
        let hi = (raw[i] as char).to_digit(16).unwrap() as u8;
        let lo = (raw[i + 1] as char).to_digit(16).unwrap() as u8;
        bytes.push((hi << 4) | lo);
        i += 2;
    }
    bytes
}
