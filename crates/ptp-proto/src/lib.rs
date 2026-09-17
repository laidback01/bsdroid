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

use std::collections::BTreeMap;
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
    /// A field holds a value the project cannot read.
    ///
    /// A parser cannot step over a value whose width it does not know, so the
    /// caller must use another way to get the same information.
    Unsupported { field: &'static str, value: u32 },
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
            Self::Unsupported { field, value } => {
                write!(
                    f,
                    "the field {field} holds {value}, which this project cannot read"
                )
            }
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

/// The header of a container, with no payload.
///
/// A large data phase does not fit in one USB transfer. The first transfer
/// holds the header and the start of the payload, and the length field says
/// how many bytes follow. A caller reads the header first, and then reads the
/// rest of the payload.
///
/// [`Container::parse`] needs the whole container, so [`Container::parse`]
/// cannot read the first transfer of a large data phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The size of the whole container, in bytes, and the header counts.
    pub length: u32,
    /// The kind of the container.
    pub kind: ContainerType,
    /// An operation code, a response code, or an event code.
    pub code: u16,
    /// The transaction id.
    pub transaction_id: u32,
}

impl Header {
    /// Reads a header from the first 12 bytes of a buffer.
    ///
    /// The function does not compare the length field with the buffer size.
    /// The buffer holds one transfer, and the container is often larger.
    pub fn parse(buf: &[u8]) -> Result<Self, ParseError> {
        if buf.len() < HEADER_LEN {
            return Err(ParseError::ShortHeader { got: buf.len() });
        }
        Ok(Self {
            length: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            kind: ContainerType::from_wire(u16::from_le_bytes([buf[4], buf[5]]))?,
            code: u16::from_le_bytes([buf[6], buf[7]]),
            transaction_id: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }

    /// The count of payload bytes the container holds.
    ///
    /// The function gives 0 if the length field is below the header size.
    pub fn payload_len(&self) -> usize {
        (self.length as usize).saturating_sub(HEADER_LEN)
    }
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

    /// Reads the payload as the parameters of a command or a response.
    ///
    /// A parameter is a `u32`, and the container holds up to five. The
    /// function ignores a byte after the last whole parameter.
    pub fn parameters(&self) -> Vec<u32> {
        self.payload
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}

/// Writes a PTP string into a buffer.
///
/// A caller outside this crate needs the function for the value of an object
/// property, which is a string with no dataset around it.
pub fn push_ptp_string(out: &mut Vec<u8>, s: &str) {
    push_string(out, s)
}

/// Writes a PTP string into a buffer.
///
/// The first byte holds the character count, and the count includes the
/// terminator. The characters are UTF-16 little-endian. An empty string gives
/// one byte of 0, and no characters.
fn push_string(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.push(0);
        return;
    }
    let units: Vec<u16> = s.encode_utf16().collect();

    // The count byte holds the characters and the terminator. A name longer
    // than 254 characters does not fit, so the function cuts the name.
    let max = 254usize;
    let take = core::cmp::min(units.len(), max);
    out.push((take + 1) as u8);
    for u in &units[..take] {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
}

/// The largest payload one container carries, in bytes.
///
/// The length field is a `u32`, and the field counts the header. A payload
/// above this count has no length that the field can hold.
pub const MAX_PAYLOAD_LEN: usize = u32::MAX as usize - HEADER_LEN;

/// The largest count of parameters a command container carries.
///
/// PTP gives a command five parameter slots.
pub const MAX_PARAMS: usize = 5;

/// A payload that does not fit the length field of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadTooLarge {
    /// The count of payload bytes the caller gave.
    pub len: u64,
}

impl fmt::Display for PayloadTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the payload holds {} bytes, and one container holds {MAX_PAYLOAD_LEN} at most",
            self.len
        )
    }
}

impl std::error::Error for PayloadTooLarge {}

/// Gives the value of the length field for a payload of this size.
///
/// The length field counts the header, so the function adds [`HEADER_LEN`].
///
/// The function gives an error for a payload the field cannot count. An
/// earlier version cast the sum to `u32` and let the sum wrap. A payload of
/// 4294967295 bytes then declared a length of 11, which is below the header
/// size, and the device read the next command as payload.
pub fn container_length(payload_len: u64) -> Result<u32, PayloadTooLarge> {
    if payload_len > MAX_PAYLOAD_LEN as u64 {
        return Err(PayloadTooLarge { len: payload_len });
    }
    Ok(HEADER_LEN as u32 + payload_len as u32)
}

/// Writes the 12 byte header of a container.
///
/// The caller gives a length that [`container_length`] checked.
fn push_header(
    out: &mut Vec<u8>,
    length: u32,
    kind: ContainerType,
    code: u16,
    transaction_id: u32,
) {
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&kind.to_wire().to_le_bytes());
    out.extend_from_slice(&code.to_le_bytes());
    out.extend_from_slice(&transaction_id.to_le_bytes());
}

/// Builds a container for the wire.
///
/// The function writes the header and then the payload. The length field
/// counts the header, which is what a device expects.
///
/// The function gives an error for a payload that the length field cannot
/// count. See [`container_length`].
pub fn build(
    kind: ContainerType,
    code: u16,
    transaction_id: u32,
    payload: &[u8],
) -> Result<Vec<u8>, PayloadTooLarge> {
    let length = container_length(payload.len() as u64)?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    push_header(&mut out, length, kind, code, transaction_id);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Builds the header of a data container whose payload follows.
///
/// A large data phase does not fit in one USB transfer. The caller sends this
/// header with the start of the payload, and then sends the rest. The length
/// field counts the header, so the caller gives the size of the payload alone.
///
/// The function gives an error for a payload that the length field cannot
/// count. See [`container_length`].
pub fn build_data_header(
    code: u16,
    transaction_id: u32,
    payload_len: u64,
) -> Result<Vec<u8>, PayloadTooLarge> {
    let length = container_length(payload_len)?;
    let mut out = Vec::with_capacity(HEADER_LEN);
    push_header(&mut out, length, ContainerType::Data, code, transaction_id);
    Ok(out)
}

/// Builds a command container with `u32` parameters.
///
/// PTP gives a command five parameter slots. The function sends the first
/// [`MAX_PARAMS`] and drops the rest, because a device rejects a container
/// with more.
///
/// The payload is therefore 20 bytes at most, and the length field always
/// holds it. The function needs no error for that reason.
pub fn build_command(code: u16, transaction_id: u32, params: &[u32]) -> Vec<u8> {
    let take = core::cmp::min(params.len(), MAX_PARAMS);
    let length = (HEADER_LEN + take * 4) as u32;

    let mut out = Vec::with_capacity(HEADER_LEN + take * 4);
    push_header(
        &mut out,
        length,
        ContainerType::Command,
        code,
        transaction_id,
    );
    for p in &params[..take] {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out
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

    /// Reads a PTP array of `u16`.
    fn read_u16_array(&mut self, field: &'static str) -> Result<Vec<u16>, ParseError> {
        let count = self.read_u32(field)? as usize;

        // Check the count against the buffer before the allocation.
        let need = count.checked_mul(2).ok_or(ParseError::Truncated {
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
            out.push(self.read_u16(field)?);
        }
        Ok(out)
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

/// Object format codes the project names.
///
/// A device uses many more codes. The list holds the codes the project needs
/// to tell a folder from a file.
pub mod format {
    /// A folder. PTP calls a folder an association.
    pub const ASSOCIATION: u16 = 0x3001;
    /// A file the device does not classify.
    pub const UNDEFINED: u16 = 0x3000;
    /// A text file.
    pub const TEXT: u16 = 0x3004;
    /// An HTML file.
    pub const HTML: u16 = 0x3005;
    /// A JPEG image.
    pub const EXIF_JPEG: u16 = 0x3801;
    /// A PNG image.
    pub const PNG: u16 = 0x380b;
    /// An MP3 file.
    pub const MP3: u16 = 0x3009;
    /// An MP4 file.
    pub const MP4: u16 = 0xb982;
}

/// Values for the third parameter of `GetObjectHandles`.
///
/// The parameter names an association, which is a folder. Two values are
/// special, and the standards do not agree about which value does what.
///
/// A measurement on a Samsung SM-S901U, with 2059 objects on the storage:
///
/// | Value      | Objects the device returned |
/// | ---------- | --------------------------- |
/// | 0x00000000 | 2059                        |
/// | 0xffffffff | 13                          |
///
/// The names below follow the measurement, and not a standard. Test a new
/// device before you trust the names. See `docs/03-object-handles.md`.
pub mod association {
    /// The value that returned every object on the test device.
    pub const EVERY_OBJECT: u32 = 0x0000_0000;
    /// The value that returned the objects of the root folder on the test
    /// device.
    pub const ROOT_ONLY: u32 = 0xffff_ffff;
}

/// What a device reports about one object.
///
/// `GetObjectInfo` answers with this dataset. An object is a file or a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The storage that holds the object.
    pub storage_id: u32,
    /// The format code. 0x3001 means a folder.
    pub object_format: u16,
    /// A device sets the field to stop a host from a write.
    pub protection_status: u16,
    /// The size of the object, in bytes.
    pub compressed_size: u32,
    /// The width of an image, in pixels. The field is 0 for a file.
    pub image_pix_width: u32,
    /// The height of an image, in pixels. The field is 0 for a file.
    pub image_pix_height: u32,
    /// The handle of the folder that holds the object.
    pub parent_object: u32,
    /// The kind of folder. The field is 0 for a file.
    pub association_type: u16,
    /// The name of the object.
    pub filename: String,
    /// The date the device made the object.
    pub capture_date: String,
    /// The date a writer last changed the object.
    pub modification_date: String,
}

impl ObjectInfo {
    /// Tells you if the object is a folder.
    pub fn is_folder(&self) -> bool {
        self.object_format == format::ASSOCIATION
    }

    /// Builds an `ObjectInfo` dataset for `SendObjectInfo`.
    ///
    /// The host sends the dataset before the bytes of a file. The dataset
    /// tells the device the name, the size and the folder.
    ///
    /// The size must be correct. A device reads exactly that count of bytes in
    /// the next operation.
    pub fn build_for_send(
        storage_id: u32,
        parent: u32,
        name: &str,
        size: u32,
        is_folder: bool,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + name.len() * 2);

        out.extend_from_slice(&storage_id.to_le_bytes());

        let format: u16 = if is_folder {
            format::ASSOCIATION
        } else {
            format::UNDEFINED
        };
        out.extend_from_slice(&format.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // protection status
        out.extend_from_slice(&size.to_le_bytes());

        // The thumbnail fields, which a host does not set.
        out.extend_from_slice(&0u16.to_le_bytes()); // thumb format
        out.extend_from_slice(&0u32.to_le_bytes()); // thumb size
        out.extend_from_slice(&0u32.to_le_bytes()); // thumb width
        out.extend_from_slice(&0u32.to_le_bytes()); // thumb height

        out.extend_from_slice(&0u32.to_le_bytes()); // image width
        out.extend_from_slice(&0u32.to_le_bytes()); // image height
        out.extend_from_slice(&0u32.to_le_bytes()); // image bit depth

        out.extend_from_slice(&parent.to_le_bytes());

        // A folder is an association, and a file is not.
        let association: u16 = if is_folder { 1 } else { 0 };
        out.extend_from_slice(&association.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // association description
        out.extend_from_slice(&0u32.to_le_bytes()); // sequence number

        push_string(&mut out, name);
        push_string(&mut out, ""); // capture date
        push_string(&mut out, ""); // modification date
        push_string(&mut out, ""); // keywords

        out
    }

    /// Reads an `ObjectInfo` dataset from the payload of a data container.
    ///
    /// The dataset holds 15 scalar fields and 4 strings. A device can stop the
    /// dataset after the third string, so the function gives an empty string
    /// for a field the device does not send.
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(payload);

        let storage_id = r.read_u32("storage_id")?;
        let object_format = r.read_u16("object_format")?;
        let protection_status = r.read_u16("protection_status")?;
        let compressed_size = r.read_u32("compressed_size")?;

        // The thumbnail fields. The project does not use them, and the fields
        // must still move the cursor.
        let _thumb_format = r.read_u16("thumb_format")?;
        let _thumb_compressed_size = r.read_u32("thumb_compressed_size")?;
        let _thumb_pix_width = r.read_u32("thumb_pix_width")?;
        let _thumb_pix_height = r.read_u32("thumb_pix_height")?;

        let image_pix_width = r.read_u32("image_pix_width")?;
        let image_pix_height = r.read_u32("image_pix_height")?;
        let _image_bit_depth = r.read_u32("image_bit_depth")?;

        let parent_object = r.read_u32("parent_object")?;
        let association_type = r.read_u16("association_type")?;
        let _association_desc = r.read_u32("association_desc")?;
        let _sequence_number = r.read_u32("sequence_number")?;

        let filename = r.read_string("filename")?;

        // A device can stop here. An absent string is not a fault.
        let capture_date = r.read_string("capture_date").unwrap_or_default();
        let modification_date = r.read_string("modification_date").unwrap_or_default();

        Ok(Self {
            storage_id,
            object_format,
            protection_status,
            compressed_size,
            image_pix_width,
            image_pix_height,
            parent_object,
            association_type,
            filename,
            capture_date,
            modification_date,
        })
    }
}

/// Operation codes that change what a filesystem can do.
///
/// A device does not support every operation. `GetDeviceInfo` gives the list,
/// and the list decides the design of a filesystem.
pub mod op {
    /// Reads part of an object. A filesystem needs this operation, because a
    /// read asks for an offset and a length.
    pub const GET_PARTIAL_OBJECT: u16 = 0x101b;
    /// Reads part of an object, with a 64 bit offset. Android adds this one.
    pub const GET_PARTIAL_OBJECT_64: u16 = 0x95c1;
    /// Writes part of an object, with a 64 bit offset.
    pub const SEND_PARTIAL_OBJECT: u16 = 0x95c2;
    /// Announces an object the host is about to send.
    pub const SEND_OBJECT_INFO: u16 = 0x100c;
    /// Sends the bytes of an object.
    pub const SEND_OBJECT: u16 = 0x100d;
    /// Removes an object.
    pub const DELETE_OBJECT: u16 = 0x100b;
    /// Moves an object to another folder.
    pub const MOVE_OBJECT: u16 = 0x1019;
    /// Copies an object.
    pub const COPY_OBJECT: u16 = 0x101a;
    /// Reads many properties of many objects in one operation. A folder list
    /// is fast with this operation, and slow without it.
    pub const GET_OBJECT_PROP_LIST: u16 = 0x9805;
    /// Lists the properties an object format supports.
    pub const GET_OBJECT_PROPS_SUPPORTED: u16 = 0x9801;
    /// Changes one property of one object. A rename needs this operation.
    pub const SET_OBJECT_PROP_VALUE: u16 = 0x9804;
    /// Changes the size of an object.
    pub const TRUNCATE_OBJECT: u16 = 0x101c;
    /// Starts a write that has no size in advance.
    pub const BEGIN_EDIT_OBJECT: u16 = 0x95c4;

    /// Gives a name for an operation code.
    pub fn name(code: u16) -> &'static str {
        match code {
            0x1001 => "GetDeviceInfo",
            0x1002 => "OpenSession",
            0x1003 => "CloseSession",
            0x1004 => "GetStorageIDs",
            0x1005 => "GetStorageInfo",
            0x1006 => "GetNumObjects",
            0x1007 => "GetObjectHandles",
            0x1008 => "GetObjectInfo",
            0x1009 => "GetObject",
            0x100a => "GetThumb",
            0x100b => "DeleteObject",
            0x100c => "SendObjectInfo",
            0x100d => "SendObject",
            0x1014 => "GetDevicePropDesc",
            0x1015 => "GetDevicePropValue",
            0x1016 => "SetDevicePropValue",
            0x1019 => "MoveObject",
            0x101a => "CopyObject",
            0x101b => "GetPartialObject",
            0x101c => "TruncateObject",
            0x9801 => "GetObjectPropsSupported",
            0x9802 => "GetObjectPropDesc",
            0x9803 => "GetObjectPropValue",
            0x9804 => "SetObjectPropValue",
            0x9805 => "GetObjectPropList",
            0x9806 => "SetObjectPropList",
            0x9808 => "SendObjectPropList",
            0x9810 => "GetObjectReferences",
            0x9811 => "SetObjectReferences",
            0x95c1 => "GetPartialObject64",
            0x95c2 => "SendPartialObject",
            0x95c3 => "TruncateObject64",
            0x95c4 => "BeginEditObject",
            0x95c5 => "EndEditObject",
            _ => "unknown",
        }
    }
}

/// What a device says about itself.
///
/// `GetDeviceInfo` answers with this dataset. The list of operations decides
/// what a filesystem on top of the device can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The version of the standard the device follows, times 100.
    pub standard_version: u16,
    /// The vendor that defines the extra operations.
    pub vendor_extension_id: u32,
    /// The version of the vendor extension, times 100.
    pub vendor_extension_version: u16,
    /// A description of the vendor extension.
    pub vendor_extension_desc: String,
    /// The operations the device supports.
    pub operations_supported: Vec<u16>,
    /// The events the device sends.
    pub events_supported: Vec<u16>,
    /// The device properties the device holds.
    pub device_properties_supported: Vec<u16>,
    /// The formats the device can make.
    pub capture_formats: Vec<u16>,
    /// The formats the device holds.
    pub image_formats: Vec<u16>,
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
}

impl DeviceInfo {
    /// Tells you if the device supports an operation.
    pub fn supports(&self, code: u16) -> bool {
        self.operations_supported.contains(&code)
    }

    /// Tells you if the host can read part of an object.
    ///
    /// A filesystem needs this answer. A read asks for an offset and a length,
    /// and `GetObject` gives the whole object. A device without a partial read
    /// makes the host read a whole file for each read of one byte.
    pub fn can_read_part(&self) -> bool {
        self.supports(op::GET_PARTIAL_OBJECT) || self.supports(op::GET_PARTIAL_OBJECT_64)
    }

    /// Tells you if the host can write a new object.
    pub fn can_write(&self) -> bool {
        self.supports(op::SEND_OBJECT_INFO) && self.supports(op::SEND_OBJECT)
    }

    /// Tells you if the host can remove an object.
    pub fn can_delete(&self) -> bool {
        self.supports(op::DELETE_OBJECT)
    }

    /// Tells you if the host can read a folder in one operation.
    pub fn has_fast_listing(&self) -> bool {
        self.supports(op::GET_OBJECT_PROP_LIST)
    }

    /// Reads a `DeviceInfo` dataset from the payload of a data container.
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let mut r = Reader::new(payload);
        Ok(Self {
            standard_version: r.read_u16("standard_version")?,
            vendor_extension_id: r.read_u32("vendor_extension_id")?,
            vendor_extension_version: r.read_u16("vendor_extension_version")?,
            vendor_extension_desc: r.read_string("vendor_extension_desc")?,
            // The functional mode is not useful here, and the field must still
            // move the cursor.
            operations_supported: {
                let _mode = r.read_u16("functional_mode")?;
                r.read_u16_array("operations_supported")?
            },
            events_supported: r.read_u16_array("events_supported")?,
            device_properties_supported: r.read_u16_array("device_properties_supported")?,
            capture_formats: r.read_u16_array("capture_formats")?,
            image_formats: r.read_u16_array("image_formats")?,
            manufacturer: r.read_string("manufacturer").unwrap_or_default(),
            model: r.read_string("model").unwrap_or_default(),
            device_version: r.read_string("device_version").unwrap_or_default(),
            serial_number: r.read_string("serial_number").unwrap_or_default(),
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

    /// A payload of `u32::MAX` bytes once wrapped the length field to 11.
    ///
    /// A length of 11 is below the header size. The device then treats the
    /// next command as payload, and the transfer never recovers.
    #[test]
    fn a_payload_that_does_not_fit_the_length_field_gives_an_error() {
        assert_eq!(
            container_length(u32::MAX as u64),
            Err(PayloadTooLarge {
                len: u32::MAX as u64
            })
        );
        assert!(container_length(MAX_PAYLOAD_LEN as u64 + 1).is_err());
        assert!(build_data_header(0x100d, 1, u32::MAX as u64).is_err());
    }

    #[test]
    fn the_largest_payload_that_fits_gives_the_largest_length() {
        assert_eq!(container_length(MAX_PAYLOAD_LEN as u64), Ok(u32::MAX));
        assert_eq!(container_length(0), Ok(HEADER_LEN as u32));
    }

    /// The length field counts the header, so a header alone declares 12 plus
    /// the payload that follows in later transfers.
    #[test]
    fn a_data_header_counts_the_payload_that_follows() {
        let h = build_data_header(0x1009, 7, 200_000).unwrap();
        assert_eq!(h.len(), HEADER_LEN, "the header carries no payload");

        let parsed = Header::parse(&h).unwrap();
        assert_eq!(parsed.length, 200_012);
        assert_eq!(parsed.payload_len(), 200_000);
        assert_eq!(parsed.kind, ContainerType::Data);
        assert_eq!(parsed.code, 0x1009);
        assert_eq!(parsed.transaction_id, 7);
    }

    /// PTP gives a command five parameter slots, so the payload cannot grow
    /// past 20 bytes and the length field always holds it.
    #[test]
    fn a_command_sends_five_parameters_at_most() {
        let all = build_command(0x1007, 3, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(all.len(), HEADER_LEN + MAX_PARAMS * 4);

        let c = Container::parse(&all).unwrap();
        assert_eq!(c.parameters(), vec![1, 2, 3, 4, 5]);
    }
}

/// Object property codes the project uses.
///
/// A device holds many more properties. The list holds the properties that
/// give a filesystem the name, the size and the place of an object.
pub mod prop {
    /// The storage that holds the object.
    pub const STORAGE_ID: u16 = 0xdc01;
    /// The format code. `format::ASSOCIATION` means a folder.
    pub const OBJECT_FORMAT: u16 = 0xdc02;
    /// The size of the object, in bytes.
    pub const OBJECT_SIZE: u16 = 0xdc04;
    /// The name of the object.
    pub const OBJECT_FILE_NAME: u16 = 0xdc07;
    /// The handle of the folder that holds the object.
    pub const PARENT_OBJECT: u16 = 0xdc0b;
    /// Asks for every property. The value goes in parameter 3.
    pub const ALL_PROPERTIES: u32 = 0xffff_ffff;
}

/// Data type codes of PTP.
///
/// A property value carries its own type, so a parser must know the width of
/// each type to step to the next entry.
pub mod datatype {
    pub const INT8: u16 = 0x0001;
    pub const UINT8: u16 = 0x0002;
    pub const INT16: u16 = 0x0003;
    pub const UINT16: u16 = 0x0004;
    pub const INT32: u16 = 0x0005;
    pub const UINT32: u16 = 0x0006;
    pub const INT64: u16 = 0x0007;
    pub const UINT64: u16 = 0x0008;
    pub const INT128: u16 = 0x0009;
    pub const UINT128: u16 = 0x000a;
    pub const STR: u16 = 0xffff;
}

/// The value of one object property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    /// A number of 64 bits or fewer.
    Number(u64),
    /// A string.
    Text(String),
    /// A value of 128 bits, which the project does not use.
    Wide,
}

/// One property of one object, from a `GetObjectPropList` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropEntry {
    pub handle: u32,
    pub code: u16,
    pub value: PropValue,
}

/// What a listing needs to know about one object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PropRecord {
    pub handle: u32,
    pub name: String,
    pub size: u64,
    pub format: u16,
    pub parent: u32,
    pub storage: u32,
}

impl PropRecord {
    /// Says whether the object is a folder.
    pub fn is_folder(&self) -> bool {
        self.format == format::ASSOCIATION
    }
}

/// Reads an `ObjectPropList` dataset.
///
/// The dataset holds a count, and then one element for each property of each
/// object. One request therefore gives the name and the size of every object in
/// a folder. The older way costs one request for each object.
///
/// The shape of one element:
///
/// ```text
/// uint32  the handle of the object
/// uint16  the property code
/// uint16  the data type
/// ...     the value, in the width the data type gives
/// ```
///
/// The function gives an error for a data type it does not know. A caller
/// cannot step over a value of unknown width, so the caller must use the older
/// way for that device.
pub fn parse_object_prop_list(data: &[u8]) -> Result<Vec<PropEntry>, ParseError> {
    let mut r = Reader::new(data);
    let count = r.read_u32("prop_list_count")? as usize;

    // A count from a device cannot set the size of an allocation, because a
    // wrong count then asks for a very large block of memory.
    let mut out: Vec<PropEntry> = Vec::new();

    for _ in 0..count {
        let handle = r.read_u32("prop_handle")?;
        let code = r.read_u16("prop_code")?;
        let kind = r.read_u16("prop_type")?;

        let value = match kind {
            datatype::INT8 | datatype::UINT8 => PropValue::Number(r.read_u8("prop_u8")? as u64),
            datatype::INT16 | datatype::UINT16 => PropValue::Number(r.read_u16("prop_u16")? as u64),
            datatype::INT32 | datatype::UINT32 => PropValue::Number(r.read_u32("prop_u32")? as u64),
            datatype::INT64 | datatype::UINT64 => PropValue::Number(r.read_u64("prop_u64")?),
            datatype::INT128 | datatype::UINT128 => {
                r.take(16, "prop_u128")?;
                PropValue::Wide
            }
            datatype::STR => PropValue::Text(r.read_string("prop_str")?),
            _ => {
                return Err(ParseError::Unsupported {
                    field: "prop_type",
                    value: kind as u32,
                })
            }
        };

        out.push(PropEntry {
            handle,
            code,
            value,
        });
    }

    Ok(out)
}

/// Collects the properties of each object into one record for each object.
///
/// The order of the answer is the order of the device. The function keeps that
/// order, because a listing then matches the older way.
pub fn fold_prop_list(entries: &[PropEntry]) -> Vec<PropRecord> {
    let mut out: Vec<PropRecord> = Vec::new();
    let mut seen: BTreeMap<u32, usize> = BTreeMap::new();

    for e in entries {
        let at = match seen.get(&e.handle) {
            Some(i) => *i,
            None => {
                out.push(PropRecord {
                    handle: e.handle,
                    ..Default::default()
                });
                seen.insert(e.handle, out.len() - 1);
                out.len() - 1
            }
        };

        let rec = &mut out[at];
        match (e.code, &e.value) {
            (prop::OBJECT_FILE_NAME, PropValue::Text(s)) => rec.name = s.clone(),
            (prop::OBJECT_SIZE, PropValue::Number(n)) => rec.size = *n,
            (prop::OBJECT_FORMAT, PropValue::Number(n)) => rec.format = *n as u16,
            (prop::PARENT_OBJECT, PropValue::Number(n)) => rec.parent = *n as u32,
            (prop::STORAGE_ID, PropValue::Number(n)) => rec.storage = *n as u32,
            _ => {}
        }
    }

    out
}

#[cfg(test)]
mod prop_list_tests {
    use super::*;

    /// Builds one element of an `ObjectPropList` dataset.
    fn element(handle: u32, code: u16, kind: u16, value: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&handle.to_le_bytes());
        v.extend_from_slice(&code.to_le_bytes());
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(value);
        v
    }

    /// Builds a PTP string: a count of characters, and then UTF-16.
    fn ptp_string(s: &str) -> Vec<u8> {
        let mut units: Vec<u16> = s.encode_utf16().collect();
        units.push(0);
        let mut v = vec![units.len() as u8];
        for u in units {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }

    fn dataset(elements: &[Vec<u8>]) -> Vec<u8> {
        let mut v = (elements.len() as u32).to_le_bytes().to_vec();
        for e in elements {
            v.extend_from_slice(e);
        }
        v
    }

    #[test]
    fn an_empty_list_gives_no_entry() {
        let data = 0u32.to_le_bytes().to_vec();
        assert_eq!(parse_object_prop_list(&data).unwrap(), Vec::new());
    }

    #[test]
    fn a_file_gives_a_name_and_a_size() {
        let data = dataset(&[
            element(
                7,
                prop::OBJECT_FILE_NAME,
                datatype::STR,
                &ptp_string("a.jpg"),
            ),
            element(
                7,
                prop::OBJECT_SIZE,
                datatype::UINT64,
                &1_048_576u64.to_le_bytes(),
            ),
            element(
                7,
                prop::OBJECT_FORMAT,
                datatype::UINT16,
                &format::EXIF_JPEG.to_le_bytes(),
            ),
            element(
                7,
                prop::PARENT_OBJECT,
                datatype::UINT32,
                &3u32.to_le_bytes(),
            ),
        ]);

        let recs = fold_prop_list(&parse_object_prop_list(&data).unwrap());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].handle, 7);
        assert_eq!(recs[0].name, "a.jpg");
        assert_eq!(recs[0].size, 1_048_576);
        assert_eq!(recs[0].parent, 3);
        assert!(!recs[0].is_folder());
    }

    #[test]
    fn the_association_format_marks_a_folder() {
        let data = dataset(&[
            element(
                9,
                prop::OBJECT_FILE_NAME,
                datatype::STR,
                &ptp_string("DCIM"),
            ),
            element(
                9,
                prop::OBJECT_FORMAT,
                datatype::UINT16,
                &format::ASSOCIATION.to_le_bytes(),
            ),
        ]);

        let recs = fold_prop_list(&parse_object_prop_list(&data).unwrap());
        assert!(recs[0].is_folder());
        assert_eq!(recs[0].name, "DCIM");
    }

    #[test]
    fn the_order_of_the_device_stays() {
        let data = dataset(&[
            element(30, prop::OBJECT_FILE_NAME, datatype::STR, &ptp_string("c")),
            element(10, prop::OBJECT_FILE_NAME, datatype::STR, &ptp_string("a")),
            element(20, prop::OBJECT_FILE_NAME, datatype::STR, &ptp_string("b")),
        ]);

        let recs = fold_prop_list(&parse_object_prop_list(&data).unwrap());
        let names: Vec<&str> = recs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    #[test]
    fn a_property_the_project_does_not_use_does_not_stop_the_parse() {
        // DateModified is a string, and the project reads no date.
        let data = dataset(&[
            element(5, 0xdc09, datatype::STR, &ptp_string("20260916T101500")),
            element(
                5,
                prop::OBJECT_FILE_NAME,
                datatype::STR,
                &ptp_string("b.mp4"),
            ),
        ]);

        let recs = fold_prop_list(&parse_object_prop_list(&data).unwrap());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].name, "b.mp4");
    }

    #[test]
    fn a_value_of_128_bits_does_not_stop_the_parse() {
        // Devices send PersistentUniqueObjectIdentifier, which is 128 bits.
        let data = dataset(&[
            element(5, 0xdc41, datatype::UINT128, &[0u8; 16]),
            element(
                5,
                prop::OBJECT_FILE_NAME,
                datatype::STR,
                &ptp_string("c.png"),
            ),
        ]);

        let recs = fold_prop_list(&parse_object_prop_list(&data).unwrap());
        assert_eq!(recs[0].name, "c.png");
    }

    #[test]
    fn a_data_type_the_project_cannot_read_gives_an_error() {
        // 0x4006 is an array of uint32. A parser cannot step over the value
        // without the count, so the caller must use the older way.
        let data = dataset(&[element(5, 0xdc07, 0x4006, &[0u8; 8])]);
        assert!(matches!(
            parse_object_prop_list(&data),
            Err(ParseError::Unsupported { .. })
        ));
    }

    #[test]
    fn a_truncated_answer_gives_an_error() {
        let mut data = dataset(&[element(
            7,
            prop::OBJECT_SIZE,
            datatype::UINT64,
            &1u64.to_le_bytes(),
        )]);
        data.truncate(data.len() - 3);
        assert!(parse_object_prop_list(&data).is_err());
    }

    #[test]
    fn a_count_that_is_too_large_gives_an_error_and_not_a_large_allocation() {
        // A device that gives a wrong count must not make the host reserve a
        // very large block of memory.
        let mut data = 0xffff_ffffu32.to_le_bytes().to_vec();
        data.extend_from_slice(&element(
            1,
            prop::OBJECT_SIZE,
            datatype::UINT32,
            &1u32.to_le_bytes(),
        ));
        assert!(parse_object_prop_list(&data).is_err());
    }

    #[test]
    fn a_size_of_more_than_4_gb_survives() {
        let data = dataset(&[element(
            7,
            prop::OBJECT_SIZE,
            datatype::UINT64,
            &5_000_000_000u64.to_le_bytes(),
        )]);
        let recs = fold_prop_list(&parse_object_prop_list(&data).unwrap());
        assert_eq!(recs[0].size, 5_000_000_000);
    }
}
