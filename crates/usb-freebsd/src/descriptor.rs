//! USB descriptor parsing.
//!
//! The module is pure. The module does no I/O, so a test runs the module with
//! no device.
//!
//! A configuration descriptor is a list of smaller descriptors. Each one
//! starts with a length byte and a type byte:
//!
//! ```text
//! bLength  bDescriptorType  ...fields...
//! ```
//!
//! An endpoint descriptor belongs to the interface descriptor before it. A
//! class descriptor can sit between them, and a class descriptor must not
//! change the owner of an endpoint.

use std::fmt;

/// Descriptor type values the module needs.
const TYPE_CONFIGURATION: u8 = 0x02;
const TYPE_INTERFACE: u8 = 0x04;
const TYPE_ENDPOINT: u8 = 0x05;

/// The interface class for MTP. The USB standard calls the class
/// "still imaging".
pub const MTP_CLASS: u8 = 0x06;
/// The interface subclass for MTP.
pub const MTP_SUBCLASS: u8 = 0x01;
/// The interface protocol for MTP.
pub const MTP_PROTOCOL: u8 = 0x01;

/// The interface class for adb. The class is a vendor class.
pub const ADB_CLASS: u8 = 0xff;
/// The interface subclass for adb.
pub const ADB_SUBCLASS: u8 = 0x42;
/// The interface protocol for adb.
pub const ADB_PROTOCOL: u8 = 0x01;

/// The endpoint transfer type for bulk. The value comes from the low two bits
/// of `bmAttributes`.
const TRANSFER_BULK: u8 = 0x02;
/// The endpoint transfer type for interrupt.
const TRANSFER_INTERRUPT: u8 = 0x03;

/// The direction bit of an endpoint address. A set bit means the endpoint
/// sends data to the host.
const DIRECTION_IN: u8 = 0x80;

/// A fault in a byte buffer that holds a descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorError {
    /// The buffer is smaller than the descriptor needs.
    TooShort { need: usize, got: usize },
    /// The type byte does not hold the expected value.
    WrongType { expected: u8, got: u8 },
    /// A descriptor gives a length of 0.
    ///
    /// A parser that advances by the length never advances. The error stops an
    /// endless loop. See rule 1 in `docs/00-why.md`.
    ZeroLength { at: usize },
    /// A descriptor declares more bytes than the buffer holds.
    RunsPastEnd { at: usize, need: usize, got: usize },
    /// An endpoint descriptor comes before any interface descriptor.
    EndpointWithoutInterface,
    /// The device gives no MTP interface.
    NoMtpInterface,
    /// The MTP interface does not hold both bulk endpoints.
    IncompleteMtpEndpoints { has_in: bool, has_out: bool },
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { need, got } => {
                write!(f, "the descriptor needs {need} bytes, and the buffer holds {got}")
            }
            Self::WrongType { expected, got } => write!(
                f,
                "the descriptor type is {got:#04x}, and the expected type is {expected:#04x}"
            ),
            Self::ZeroLength { at } => {
                write!(f, "the descriptor at offset {at} gives a length of 0")
            }
            Self::RunsPastEnd { at, need, got } => write!(
                f,
                "the descriptor at offset {at} needs {need} bytes, and {got} bytes remain"
            ),
            Self::EndpointWithoutInterface => {
                write!(f, "an endpoint comes before any interface")
            }
            Self::NoMtpInterface => write!(f, "the device gives no MTP interface"),
            Self::IncompleteMtpEndpoints { has_in, has_out } => write!(
                f,
                "the MTP interface needs two bulk endpoints. Data from device: {has_in}. Data to device: {has_out}"
            ),
        }
    }
}

impl std::error::Error for DescriptorError {}

/// One endpoint of an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The endpoint address. Bit 0x80 means the endpoint sends to the host.
    pub address: u8,
    /// The transfer type. 2 means bulk, and 3 means interrupt.
    pub transfer_type: u8,
    /// The largest packet the endpoint accepts, in bytes.
    pub max_packet_size: u16,
}

impl Endpoint {
    /// Tells you if the endpoint sends data to the host.
    pub fn is_in(&self) -> bool {
        self.address & DIRECTION_IN != 0
    }

    /// Tells you if the endpoint is a bulk endpoint.
    pub fn is_bulk(&self) -> bool {
        self.transfer_type == TRANSFER_BULK
    }

    /// Tells you if the endpoint is an interrupt endpoint.
    pub fn is_interrupt(&self) -> bool {
        self.transfer_type == TRANSFER_INTERRUPT
    }
}

/// One interface of a configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// The index of the string that names the interface. A value of 0 means
    /// that the device gives no name.
    ///
    /// The field separates two modes that share the class. A Samsung
    /// SM-S901U gives 5 in file transfer mode, and 0 in image transfer mode.
    /// See `docs/02-device-states.md`.
    pub string_index: u8,
    pub endpoints: Vec<Endpoint>,
}

impl Interface {
    /// Tells you if the interface carries MTP.
    pub fn is_mtp(&self) -> bool {
        self.class == MTP_CLASS && self.subclass == MTP_SUBCLASS && self.protocol == MTP_PROTOCOL
    }

    /// Tells you if the interface carries adb.
    ///
    /// The interface is a sign that the device runs Android. A device that
    /// shows adb but shows no MTP is an Android device that is not in file
    /// transfer mode. The difference matters for a fault report.
    pub fn is_adb(&self) -> bool {
        self.class == ADB_CLASS && self.subclass == ADB_SUBCLASS && self.protocol == ADB_PROTOCOL
    }
}

/// A parsed configuration descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDescriptor {
    /// The size of the full descriptor, in bytes.
    pub total_length: u16,
    /// The count of interfaces the header declares.
    pub num_interfaces: u8,
    /// The value that selects this configuration.
    pub configuration_value: u8,
    /// The interfaces, in the order the device gives them.
    pub interfaces: Vec<Interface>,
}

impl ConfigDescriptor {
    /// Reads a configuration descriptor from a byte buffer.
    ///
    /// The loop advances by the length of each descriptor. A length of 0 gives
    /// an error, so the loop always advances and always stops.
    pub fn parse(buf: &[u8]) -> Result<Self, DescriptorError> {
        if buf.len() < 9 {
            return Err(DescriptorError::TooShort {
                need: 9,
                got: buf.len(),
            });
        }
        if buf[1] != TYPE_CONFIGURATION {
            return Err(DescriptorError::WrongType {
                expected: TYPE_CONFIGURATION,
                got: buf[1],
            });
        }

        let total_length = u16::from_le_bytes([buf[2], buf[3]]);
        let num_interfaces = buf[4];
        let configuration_value = buf[5];

        // Read no further than the buffer, and no further than the header
        // declares. The smaller of the two wins.
        let end = core::cmp::min(buf.len(), total_length as usize);

        let mut interfaces: Vec<Interface> = Vec::new();
        let mut pos = buf[0] as usize;
        if pos == 0 {
            return Err(DescriptorError::ZeroLength { at: 0 });
        }

        while pos + 2 <= end {
            let len = buf[pos] as usize;
            let kind = buf[pos + 1];

            if len == 0 {
                return Err(DescriptorError::ZeroLength { at: pos });
            }
            if pos + len > end {
                return Err(DescriptorError::RunsPastEnd {
                    at: pos,
                    need: len,
                    got: end - pos,
                });
            }

            match kind {
                TYPE_INTERFACE => {
                    if len < 9 {
                        return Err(DescriptorError::TooShort { need: 9, got: len });
                    }
                    interfaces.push(Interface {
                        number: buf[pos + 2],
                        alternate_setting: buf[pos + 3],
                        class: buf[pos + 5],
                        subclass: buf[pos + 6],
                        protocol: buf[pos + 7],
                        string_index: buf[pos + 8],
                        endpoints: Vec::new(),
                    });
                }
                TYPE_ENDPOINT => {
                    if len < 7 {
                        return Err(DescriptorError::TooShort { need: 7, got: len });
                    }
                    let ep = Endpoint {
                        address: buf[pos + 2],
                        transfer_type: buf[pos + 3] & 0x03,
                        max_packet_size: u16::from_le_bytes([buf[pos + 4], buf[pos + 5]]),
                    };
                    match interfaces.last_mut() {
                        Some(iface) => iface.endpoints.push(ep),
                        None => return Err(DescriptorError::EndpointWithoutInterface),
                    }
                }
                // A class descriptor or a vendor descriptor. Step over it. The
                // descriptor must not change the owner of the next endpoint.
                _ => {}
            }

            // `len` is at least 1 here, so the position always grows.
            pos += len;
        }

        Ok(Self {
            total_length,
            num_interfaces,
            configuration_value,
            interfaces,
        })
    }
}

/// The MTP interface of a device, and the endpoints the interface needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtpInterface {
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// The endpoint that sends data to the host.
    pub bulk_in: u8,
    /// The endpoint that sends data to the device.
    pub bulk_out: u8,
    /// The endpoint that reports an event. Some devices give no such endpoint.
    pub interrupt_in: Option<u8>,
    /// The largest packet the bulk endpoints accept, in bytes.
    pub max_packet_size: u16,
}

impl MtpInterface {
    /// Finds the MTP interface of a configuration.
    ///
    /// The function checks the interface class first. A phone with USB
    /// debugging on gives other interfaces that also have bulk endpoints, and
    /// the function must not choose one of them.
    pub fn find(cfg: &ConfigDescriptor) -> Result<Self, DescriptorError> {
        let iface = cfg
            .interfaces
            .iter()
            .find(|i| i.is_mtp())
            .ok_or(DescriptorError::NoMtpInterface)?;

        let bulk_in = iface.endpoints.iter().find(|e| e.is_bulk() && e.is_in());
        let bulk_out = iface.endpoints.iter().find(|e| e.is_bulk() && !e.is_in());

        let (bulk_in, bulk_out) = match (bulk_in, bulk_out) {
            (Some(i), Some(o)) => (i, o),
            (i, o) => {
                return Err(DescriptorError::IncompleteMtpEndpoints {
                    has_in: i.is_some(),
                    has_out: o.is_some(),
                })
            }
        };

        let interrupt_in = iface
            .endpoints
            .iter()
            .find(|e| e.is_interrupt() && e.is_in())
            .map(|e| e.address);

        Ok(Self {
            interface_number: iface.number,
            alternate_setting: iface.alternate_setting,
            class: iface.class,
            subclass: iface.subclass,
            protocol: iface.protocol,
            bulk_in: bulk_in.address,
            bulk_out: bulk_out.address,
            interrupt_in,
            max_packet_size: bulk_in.max_packet_size,
        })
    }
}

/// Descriptor type for the binary device object store.
const TYPE_BOS: u8 = 0x0f;
/// Descriptor type for one device capability.
const TYPE_DEVICE_CAPABILITY: u8 = 0x10;
/// Capability type for super speed.
const CAPABILITY_SUPER_SPEED: u8 = 0x03;
/// Capability type for super speed plus.
const CAPABILITY_SUPER_SPEED_PLUS: u8 = 0x0a;

/// The bit of `wSpeedsSupported` that means super speed.
const SPEED_BIT_SUPER: u16 = 0x0008;

/// What a device says it can do, from the BOS descriptor.
///
/// BOS means binary device object store. The list does not change with the
/// speed of the link, so the list tells a host what the device could do on a
/// better cable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCapabilities {
    /// The device holds a super speed capability descriptor.
    pub super_speed: bool,
    /// The device holds a super speed plus capability descriptor.
    pub super_speed_plus: bool,
    /// The raw value of `wSpeedsSupported`, if the device gives one.
    pub speeds_supported: Option<u16>,
}

impl DeviceCapabilities {
    /// Reads a BOS descriptor.
    ///
    /// The loop advances by the length of each capability. A length of 0 gives
    /// an error, so the loop always stops. See rule 1 in `docs/00-why.md`.
    pub fn parse(buf: &[u8]) -> Result<Self, DescriptorError> {
        if buf.len() < 5 {
            return Err(DescriptorError::TooShort {
                need: 5,
                got: buf.len(),
            });
        }
        if buf[1] != TYPE_BOS {
            return Err(DescriptorError::WrongType {
                expected: TYPE_BOS,
                got: buf[1],
            });
        }

        let total = u16::from_le_bytes([buf[2], buf[3]]) as usize;
        let end = core::cmp::min(buf.len(), total);

        let mut out = Self {
            super_speed: false,
            super_speed_plus: false,
            speeds_supported: None,
        };

        let mut pos = buf[0] as usize;
        if pos == 0 {
            return Err(DescriptorError::ZeroLength { at: 0 });
        }

        while pos + 3 <= end {
            let len = buf[pos] as usize;
            if len == 0 {
                return Err(DescriptorError::ZeroLength { at: pos });
            }
            if pos + len > end {
                return Err(DescriptorError::RunsPastEnd {
                    at: pos,
                    need: len,
                    got: end - pos,
                });
            }

            if buf[pos + 1] == TYPE_DEVICE_CAPABILITY {
                match buf[pos + 2] {
                    CAPABILITY_SUPER_SPEED => {
                        out.super_speed = true;
                        // wSpeedsSupported sits at offset 4 of the capability.
                        if len >= 6 {
                            out.speeds_supported =
                                Some(u16::from_le_bytes([buf[pos + 4], buf[pos + 5]]));
                        }
                    }
                    CAPABILITY_SUPER_SPEED_PLUS => out.super_speed_plus = true,
                    _ => {}
                }
            }

            pos += len;
        }

        Ok(out)
    }

    /// Tells you if the device can run faster than high speed.
    pub fn supports_faster_than_high(&self) -> bool {
        if self.super_speed_plus || self.super_speed {
            return true;
        }
        match self.speeds_supported {
            Some(s) => s & SPEED_BIT_SUPER != 0,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal configuration with one MTP interface and two bulk endpoints.
    fn minimal_mtp() -> Vec<u8> {
        let mut b = vec![0x09, 0x02, 0x00, 0x00, 0x01, 0x01, 0x00, 0x80, 0x32];
        b.extend_from_slice(&[0x09, 0x04, 0x00, 0x00, 0x02, 0x06, 0x01, 0x01, 0x00]);
        b.extend_from_slice(&[0x07, 0x05, 0x81, 0x02, 0x00, 0x02, 0x00]);
        b.extend_from_slice(&[0x07, 0x05, 0x01, 0x02, 0x00, 0x02, 0x00]);
        let total = b.len() as u16;
        b[2..4].copy_from_slice(&total.to_le_bytes());
        b
    }

    #[test]
    fn a_minimal_configuration_gives_an_mtp_interface() {
        let cfg = ConfigDescriptor::parse(&minimal_mtp()).unwrap();
        let mtp = MtpInterface::find(&cfg).unwrap();
        assert_eq!(mtp.bulk_in, 0x81);
        assert_eq!(mtp.bulk_out, 0x01);
        assert_eq!(mtp.interrupt_in, None, "the device gives no event endpoint");
    }

    #[test]
    fn an_mtp_interface_with_one_bulk_endpoint_is_incomplete() {
        let mut b = minimal_mtp();
        b.truncate(b.len() - 7); // Remove the endpoint that sends to the device.
        let total = b.len() as u16;
        b[2..4].copy_from_slice(&total.to_le_bytes());

        let cfg = ConfigDescriptor::parse(&b).unwrap();
        assert_eq!(
            MtpInterface::find(&cfg),
            Err(DescriptorError::IncompleteMtpEndpoints {
                has_in: true,
                has_out: false
            })
        );
    }

    #[test]
    fn a_header_length_of_zero_is_an_error() {
        let mut b = minimal_mtp();
        b[0] = 0x00;
        assert_eq!(
            ConfigDescriptor::parse(&b),
            Err(DescriptorError::ZeroLength { at: 0 })
        );
    }

    #[test]
    fn a_total_length_larger_than_the_buffer_reads_only_the_buffer() {
        // A device that declares more than it sends must not make the host
        // read past the buffer.
        let mut b = minimal_mtp();
        b[2..4].copy_from_slice(&9999u16.to_le_bytes());
        let cfg = ConfigDescriptor::parse(&b).expect("the parser stops at the buffer");
        assert_eq!(cfg.interfaces.len(), 1);
    }

    #[test]
    fn the_direction_bit_decides_the_direction() {
        let ep_in = Endpoint {
            address: 0x81,
            transfer_type: TRANSFER_BULK,
            max_packet_size: 512,
        };
        let ep_out = Endpoint {
            address: 0x01,
            transfer_type: TRANSFER_BULK,
            max_packet_size: 512,
        };
        assert!(ep_in.is_in());
        assert!(!ep_out.is_in());
        assert!(ep_in.is_bulk() && ep_out.is_bulk());
    }
}
