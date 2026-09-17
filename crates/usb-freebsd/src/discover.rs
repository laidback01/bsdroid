//! Finding the devices that offer an MTP interface.
//!
//! # Why the module exists
//!
//! An earlier version of this project wrote this search five times: three in
//! `mtpprobe` and two in `mtpfs`, two of them about sixty lines apart in one
//! file. Each copy did the same six steps, and each copy held its own copy of
//! the closure that reads an interface name.
//!
//! The steps are always the same:
//!
//! 1. Walk the devices of the backend.
//! 2. Open each one. A device the host cannot open is not a fault, because
//!    many devices belong to a kernel driver.
//! 3. Read the configuration descriptor.
//! 4. Parse it.
//! 5. Read the name of each interface that could be MTP with a vendor class.
//! 6. Choose the MTP interface, by class or by name.
//!
//! Step 5 is the part that needs a device, which is why
//! [`MtpInterface::find_with_names`] takes a closure and this module supplies
//! it.

use std::rc::Rc;
use std::time::Duration;

use crate::descriptor::{ConfigDescriptor, MtpInterface};
use crate::device::{Backend, DeviceHandle, OpenDevice};

/// A bus number and an address on that bus, as `ugen0.11` names them.
pub type Node = (u8, u8);

/// A device that offers an MTP interface.
pub struct Found {
    /// The device, open and ready.
    pub device: OpenDevice,
    /// The MTP interface, with both bulk endpoints.
    pub iface: MtpInterface,
    /// The configuration descriptor, for a caller that reports what it saw.
    pub config: ConfigDescriptor,
    pub bus: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
}

impl Found {
    /// The node name, in the form the mount programs of FreeBSD use.
    pub fn node(&self) -> String {
        format!("ugen{}.{}", self.bus, self.address)
    }

    /// The maker and the model, for a person to read.
    ///
    /// String 1 holds the maker and string 2 holds the model. A device that
    /// gives neither gives an empty name.
    pub fn name(&mut self) -> String {
        let maker = self.device.string_descriptor(1).unwrap_or_default();
        let model = self.device.string_descriptor(2).unwrap_or_default();
        format!("{maker} {model}").trim().to_string()
    }
}

/// Looks at one device, and reports whether it offers MTP.
///
/// The function gives `None` for a device the host cannot open, for a
/// descriptor that does not parse, and for a device with no MTP interface.
/// None of those is a fault: most devices on a bus are not cellphones.
pub fn inspect(handle: DeviceHandle, timeout: Duration) -> Option<Found> {
    let (vendor_id, product_id) = handle.ids();
    let bus = handle.bus();
    let address = handle.address();

    // Four transfer slots. MTP needs two, and the spare costs nothing.
    let mut device = handle.open(4).ok()?;
    let raw = device.config_descriptor_raw(timeout).ok()?;
    let config = ConfigDescriptor::parse(&raw).ok()?;

    // Read the name of an interface when the class alone does not say that the
    // interface carries MTP. A Motorola Moto G (5) needs the name, and a
    // Samsung SM-S901U does not.
    //
    // `find_with_names` borrows the closure, and the closure would need the
    // device, so read every name the search could want first.
    let mut names: Vec<(u8, Option<String>)> = Vec::new();
    for i in config
        .interfaces
        .iter()
        .filter(|i| i.is_vendor_mtp_candidate())
    {
        let name = device.string_descriptor(i.string_index);
        names.push((i.string_index, name));
    }

    let iface = MtpInterface::find_with_names(&config, |idx| {
        names
            .iter()
            .find(|(i, _)| *i == idx)
            .and_then(|(_, n)| n.clone())
    })
    .ok()?;

    Some(Found {
        device,
        iface,
        config,
        bus,
        address,
        vendor_id,
        product_id,
    })
}

/// Lists every device that offers an MTP interface.
pub fn list(backend: &Rc<Backend>, timeout: Duration) -> Vec<Found> {
    backend
        .devices()
        .into_iter()
        .filter_map(|d| inspect(d, timeout))
        .collect()
}

/// Finds one device that offers an MTP interface.
///
/// `node` names a bus and an address, as `parse_node` reads them from a name
/// such as `ugen0.11`. A value of `None` takes the first device the host
/// finds.
///
/// The function stops at the first match, so it opens no device it does not
/// need.
pub fn find(backend: &Rc<Backend>, node: Option<Node>, timeout: Duration) -> Option<Found> {
    for handle in backend.devices() {
        // A caller that names a device gets that device, and no other. The
        // check comes before the open, because an open costs time.
        if let Some((bus, address)) = node {
            if handle.bus() != bus || handle.address() != address {
                continue;
            }
        }
        if let Some(found) = inspect(handle, timeout) {
            return Some(found);
        }
    }
    None
}

/// Tells you if any device offers an MTP interface.
pub fn any(backend: &Rc<Backend>, timeout: Duration) -> bool {
    find(backend, None, timeout).is_some()
}

/// Lists the devices that show an adb interface and no MTP interface.
///
/// An Android device that is not in file transfer mode still shows adb, if the
/// user turned on USB debugging. The adb interface is therefore a sign that
/// the cable works and the device is awake, and that the mode is wrong. A
/// fault report needs that difference.
pub fn android_without_mtp(backend: &Rc<Backend>, timeout: Duration) -> Vec<(u8, u8, u16, u16)> {
    let mut out = Vec::new();

    for handle in backend.devices() {
        let (vid, pid) = handle.ids();
        let bus = handle.bus();
        let address = handle.address();

        let Ok(mut device) = handle.open(4) else {
            continue;
        };
        let Ok(raw) = device.config_descriptor_raw(timeout) else {
            continue;
        };
        let Ok(config) = ConfigDescriptor::parse(&raw) else {
            continue;
        };

        if config.interfaces.iter().any(|i| i.is_adb())
            && !config.interfaces.iter().any(|i| i.is_mtp())
        {
            out.push((bus, address, vid, pid));
        }
    }
    out
}

/// Reads a node name, and gives the bus and the address.
///
/// The function takes `ugen0.11` and `/dev/ugen0.11`, which are the two forms
/// a person writes.
pub fn parse_node(s: &str) -> Option<Node> {
    let s = s.strip_prefix("/dev/").unwrap_or(s);
    let s = s.strip_prefix("ugen")?;
    let (bus, addr) = s.split_once('.')?;
    Some((bus.parse().ok()?, addr.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::parse_node;

    #[test]
    fn a_node_name_reads_in_both_forms() {
        assert_eq!(parse_node("ugen0.11"), Some((0, 11)));
        assert_eq!(parse_node("/dev/ugen0.11"), Some((0, 11)));
        assert_eq!(parse_node("ugen1.3"), Some((1, 3)));
    }

    #[test]
    fn a_name_that_is_not_a_node_gives_nothing() {
        assert_eq!(parse_node("allow_other"), None);
        assert_eq!(parse_node("/mnt/phone"), None);
        assert_eq!(parse_node("ugen0"), None);
        assert_eq!(parse_node("ugen"), None);
        assert_eq!(parse_node(""), None);
        assert_eq!(parse_node("ugen0.999"), None, "an address is one byte");
    }
}
