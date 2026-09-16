//! PTP/MTP wire format.
//!
//! The crate does no I/O and has no dependency. A test runs the crate without
//! an attached device.
//!
//! # Design rules
//!
//! This project exists because an MTP stack on FreeBSD entered an endless
//! loop. See `docs/00-why.md`. Two rules come from that defect:
//!
//! 1. A function in this crate always stops. No loop depends on data from the
//!    device for its end condition.
//! 2. A function in this crate never panics on damaged input. A parse function
//!    returns [`ParseError`].
//!
//! The crate forbids unsafe code, so a damaged length field cannot read past
//! the end of a buffer.

#![forbid(unsafe_code)]

use std::fmt;

/// The size of a PTP container header, in bytes.
pub const HEADER_LEN: usize = 12;

/// A fault in a byte buffer that holds PTP data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The buffer is smaller than a container header.
    ShortHeader { got: usize },
    /// The length field and the buffer size disagree.
    LengthMismatch { declared: u32, available: usize },
    /// The container type field holds a value the standard does not define.
    UnknownContainerType(u16),
    /// A field needs more bytes than the buffer holds.
    Truncated {
        field: &'static str,
        need: usize,
        got: usize,
    },
    /// A string field does not hold valid UTF-16.
    BadString { field: &'static str },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortHeader { got } => write!(
                f,
                "the buffer holds {got} bytes, and a container header needs {HEADER_LEN}"
            ),
            Self::LengthMismatch {
                declared,
                available,
            } => write!(
                f,
                "the container declares {declared} bytes, and the buffer holds {available}"
            ),
            Self::UnknownContainerType(v) => {
                write!(f, "the container type {v} is not a defined type")
            }
            Self::Truncated { field, need, got } => write!(
                f,
                "the field {field} needs {need} bytes, and {got} bytes remain"
            ),
            Self::BadString { field } => {
                write!(f, "the field {field} does not hold valid UTF-16")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// The kind of a PTP container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerType {
    /// The host asks the device to do an operation.
    Command,
    /// The container carries the payload of an operation.
    Data,
    /// The device reports the result of an operation.
    Response,
    /// The device reports a change that no operation asked for.
    Event,
}

impl ContainerType {
    /// Converts the wire value to a container type.
    pub fn from_wire(v: u16) -> Result<Self, ParseError> {
        match v {
            1 => Ok(Self::Command),
            2 => Ok(Self::Data),
            3 => Ok(Self::Response),
            4 => Ok(Self::Event),
            other => Err(ParseError::UnknownContainerType(other)),
        }
    }

    /// Converts the container type to the wire value.
    pub fn to_wire(self) -> u16 {
        match self {
            Self::Command => 1,
            Self::Data => 2,
            Self::Response => 3,
            Self::Event => 4,
        }
    }
}

/// A PTP container, which is the unit of transfer on the wire.
///
/// The header holds 12 bytes:
///
/// | Offset | Size | Field          |
/// | ------ | ---- | -------------- |
/// | 0      | 4    | length         |
/// | 4      | 2    | container type |
/// | 6      | 2    | code           |
/// | 8      | 4    | transaction id |
///
/// Every field is little-endian. The length field counts the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container<'a> {
    /// The value of the length field, which counts the header.
    pub length: u32,
    /// The kind of the container.
    pub kind: ContainerType,
    /// An operation code, a response code, or an event code. The meaning
    /// depends on `kind`.
    pub code: u16,
    /// The transaction id, which pairs a response with its command.
    pub transaction_id: u32,
    /// The bytes after the header.
    pub payload: &'a [u8],
}

impl<'a> Container<'a> {
    /// Reads a container from a byte buffer.
    ///
    /// The function checks the length field against the buffer size. The check
    /// stops a damaged device from making the host read past the buffer.
    pub fn parse(buf: &'a [u8]) -> Result<Self, ParseError> {
        if buf.len() < HEADER_LEN {
            return Err(ParseError::ShortHeader { got: buf.len() });
        }

        let length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let kind = ContainerType::from_wire(u16::from_le_bytes([buf[4], buf[5]]))?;
        let code = u16::from_le_bytes([buf[6], buf[7]]);
        let transaction_id = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);

        // The length must cover the header, and the buffer must hold the
        // bytes the length declares.
        let declared = length as usize;
        if declared < HEADER_LEN || declared > buf.len() {
            return Err(ParseError::LengthMismatch {
                declared: length,
                available: buf.len(),
            });
        }

        Ok(Self {
            length,
            kind,
            code,
            transaction_id,
            payload: &buf[HEADER_LEN..declared],
        })
    }

    /// Reads the payload as a PTP array of `u32`.
    ///
    /// A PTP array starts with a `u32` element count. `GetStorageIDs` and
    /// `GetObjectHandles` both answer with this shape.
    pub fn payload_as_u32_array(&self) -> Result<Vec<u32>, ParseError> {
        Reader::new(self.payload).read_u32_array("array")
    }
}

/// A cursor that reads fields from a buffer and checks every read.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Takes `n` bytes, or reports how many bytes remain.
    fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], ParseError> {
        if self.remaining() < n {
            return Err(ParseError::Truncated {
                field,
                need: n,
                got: self.remaining(),
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn read_u8(&mut self, field: &'static str) -> Result<u8, ParseError> {
        Ok(self.take(1, field)?[0])
    }

    fn read_u16(&mut self, field: &'static str) -> Result<u16, ParseError> {
        let b = self.take(2, field)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32(&mut self, field: &'static str) -> Result<u32, ParseError> {
        let b = self.take(4, field)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u64(&mut self, field: &'static str) -> Result<u64, ParseError> {
        let b = self.take(8, field)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Reads a PTP string.
    ///
    /// The first byte holds the character count, and the count includes the
    /// terminator. The characters are UTF-16 little-endian. A count of 0 means
    /// an empty string, and no bytes follow.
    fn read_string(&mut self, field: &'static str) -> Result<String, ParseError> {
        let count = self.read_u8(field)? as usize;
        if count == 0 {
            return Ok(String::new());
        }

        // The count is a u8, so `count * 2` cannot overflow. `take` then
        // checks the count against the buffer.
        let bytes = self.take(count * 2, field)?;
        let mut units: Vec<u16> = Vec::with_capacity(count);
        for pair in bytes.chunks_exact(2) {
            units.push(u16::from_le_bytes([pair[0], pair[1]]));
        }
        if units.last() == Some(&0) {
            units.pop();
        }
        String::from_utf16(&units).map_err(|_| ParseError::BadString { field })
    }

    /// Reads a PTP array of `u32`.
    fn read_u32_array(&mut self, field: &'static str) -> Result<Vec<u32>, ParseError> {
        let count = self.read_u32(field)? as usize;

        // Check the count against the buffer before the allocation. A damaged
        // count of 0xffffffff must not make the host reserve 16 GiB.
        let need = count.checked_mul(4).ok_or(ParseError::Truncated {
            field,
            need: usize::MAX,
            got: self.remaining(),
        })?;
        if self.remaining() < need {
            return Err(ParseError::Truncated {
                field,
                need,
                got: self.remaining(),
            });
        }

        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.read_u32(field)?);
        }
        Ok(out)
    }
}

/// What a device reports about one storage.
///
/// `GetStorageInfo` answers with this dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInfo {
    /// 0x0003 means fixed RAM storage.
    pub storage_type: u16,
    /// 0x0002 means a generic hierarchical filesystem.
    pub filesystem_type: u16,
    /// 0x0000 means read and write.
    pub access_capability: u16,
    /// The size of the storage, in bytes.
    pub max_capacity: u64,
    /// The free space, in bytes.
    pub free_space_in_bytes: u64,
    /// The count of objects the storage can still hold.
    pub free_space_in_objects: u32,
    /// A name for the storage, such as `Internal storage`.
    pub storage_description: String,
    /// A volume label. Many devices leave the label empty.
    pub volume_identifier: String,
}

impl StorageInfo {
    /// Reads a `StorageInfo` dataset from the payload of a data container.
    ///
    /// The function ignores a byte after the dataset. Some devices append a
    /// vendor field, and an unknown trailing byte is not a fault.
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(payload);
        Ok(Self {
            storage_type: r.read_u16("storage_type")?,
            filesystem_type: r.read_u16("filesystem_type")?,
            access_capability: r.read_u16("access_capability")?,
            max_capacity: r.read_u64("max_capacity")?,
            free_space_in_bytes: r.read_u64("free_space_in_bytes")?,
            free_space_in_objects: r.read_u32("free_space_in_objects")?,
            storage_description: r.read_string("storage_description")?,
            volume_identifier: r.read_string("volume_identifier")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_type_survives_a_round_trip() {
        for t in [
            ContainerType::Command,
            ContainerType::Data,
            ContainerType::Response,
            ContainerType::Event,
        ] {
            assert_eq!(ContainerType::from_wire(t.to_wire()), Ok(t));
        }
    }

    #[test]
    fn container_type_rejects_zero_and_five() {
        assert_eq!(
            ContainerType::from_wire(0),
            Err(ParseError::UnknownContainerType(0))
        );
        assert_eq!(
            ContainerType::from_wire(5),
            Err(ParseError::UnknownContainerType(5))
        );
    }

    #[test]
    fn a_huge_array_count_does_not_reserve_memory() {
        // The payload claims 0xffffffff elements and holds none. The reader
        // must report a fault instead of reserving 16 GiB.
        let payload = [0xff, 0xff, 0xff, 0xff];
        let got = Reader::new(&payload).read_u32_array("array");
        assert!(matches!(got, Err(ParseError::Truncated { .. })), "{got:?}");
    }

    #[test]
    fn an_empty_string_reads_as_empty() {
        let mut r = Reader::new(&[0x00]);
        assert_eq!(r.read_string("s").unwrap(), "");
    }

    #[test]
    fn a_reader_never_moves_past_the_end() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert!(r.read_u64("wide").is_err());
        // The failed read must leave the cursor where it was.
        assert_eq!(r.remaining(), 3);
    }
}
