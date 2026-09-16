//! `mtpprobe` reports what an Android device does over MTP.
//!
//! The program answers one question: does the device work, and if not, at
//! which step does the device stop? A user who reports a fault should not need
//! a system call trace.
//!
//! Every step has a deadline. The program always stops.

mod session;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ptp_proto::StorageInfo;
use usb_freebsd::descriptor::{ConfigDescriptor, MtpInterface};
use usb_freebsd::device::Backend;

use session::{response_name, Session};

/// The deadline for one transfer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The buffer size for a bulk endpoint, in bytes.
const BULK_BUFFER: u32 = 16 * 1024;

fn main() -> ExitCode {
    println!("mtpprobe {}", env!("CARGO_PKG_VERSION"));
    println!("timeout per transfer: {} ms", TIMEOUT.as_millis());
    println!();

    match run() {
        Ok(()) => {
            println!();
            println!("RESULT: the device answers over MTP.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!();
            println!("RESULT: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let devices = backend.devices();
    println!("The host sees {} USB devices.", devices.len());

    // Find a device that gives an MTP interface.
    let mut chosen = None;
    for d in devices {
        let (vid, pid) = d.ids();
        let bus = d.bus();
        let addr = d.address();

        let mut open = match d.open(4) {
            Ok(o) => o,
            // A device the host cannot open is not a fault here. Many devices
            // belong to a kernel driver.
            Err(_) => continue,
        };

        let raw = match open.config_descriptor_raw(TIMEOUT) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let cfg = match ConfigDescriptor::parse(&raw) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Ok(iface) = MtpInterface::find(&cfg) {
            println!();
            println!("MTP device found:");
            println!("  bus {bus}, address {addr}");
            println!("  vendor {vid:#06x}, product {pid:#06x}");
            println!("  interface {}", iface.interface_number);
            println!("  bulk in   {:#04x}", iface.bulk_in);
            println!("  bulk out  {:#04x}", iface.bulk_out);
            match iface.interrupt_in {
                Some(e) => println!("  event in  {e:#04x}"),
                None => println!("  event in  none"),
            }
            println!("  packet size {} bytes", iface.max_packet_size);
            chosen = Some((open, iface));
            break;
        }
    }

    let (mut open, iface) = chosen.ok_or_else(|| {
        "no device gives an MTP interface. Connect the phone, unlock the phone, \
         and choose File transfer on the phone."
            .to_string()
    })?;

    if open.kernel_driver_active(iface.interface_number) {
        println!("  a kernel driver holds the interface, and the probe detaches it");
        let _ = open.detach_kernel_driver(iface.interface_number);
    }

    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the MTP endpoints: {e}"))?;

    let mut s = Session::new(channels, TIMEOUT);

    println!();
    println!("--- Session 1 ---");
    step(&mut s, "OpenSession", session::OP_OPEN_SESSION, &[1])?;
    let ids = step(&mut s, "GetStorageIDs", session::OP_GET_STORAGE_IDS, &[])?;

    let storage_ids: Vec<u32> = ids
        .data
        .get(4..)
        .unwrap_or(&[])
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    if storage_ids.is_empty() {
        println!("      storages: none");
        println!();
        println!("    The device answered OK and reported 0 storages.");
        println!("    The transport works. The device gives no file access.");
        println!();
        println!("    An Android device reports 0 storages when one of these is true:");
        println!("      - The screen is locked. Unlock the phone.");
        println!("      - The USB mode is not File transfer. Open the USB");
        println!("        notification on the phone and choose File transfer.");
        println!("      - The phone asks permission, and nobody answered yet.");
        println!();
        println!("    Correct the phone, then run mtpprobe again.");
        let _ = s.close_session();
        return Err("the device reports 0 storages".to_string());
    }

    println!("      storages: {storage_ids:?}");

    for id in &storage_ids {
        let info = step(
            &mut s,
            "GetStorageInfo",
            session::OP_GET_STORAGE_INFO,
            &[*id],
        )?;
        match StorageInfo::parse(&info.data) {
            Ok(si) => {
                println!("      description: {}", si.storage_description);
                println!("      capacity:    {} bytes", si.max_capacity);
                println!("      free:        {} bytes", si.free_space_in_bytes);
            }
            Err(e) => println!("      the payload does not parse: {e}"),
        }
    }

    step(&mut s, "CloseSession", session::OP_CLOSE_SESSION, &[])?;

    // --- The step that matters ---
    //
    // `simple-mtpfs` opens a session, closes the session, and opens a second
    // session. The second open is where the host stops. See docs/00-why.md.
    println!();
    println!("--- Session 2: the sequence that stops simple-mtpfs and jmtpfs ---");
    println!("    A second OpenSession follows a CloseSession.");

    let started = Instant::now();
    match s.open_session(2) {
        Ok(o) => {
            println!(
                "    OpenSession      {:>6} ms   {:#06x} {}",
                o.elapsed.as_millis(),
                o.response_code,
                response_name(o.response_code)
            );
            println!();
            println!("    The second session opened. The host did not stop.");
            let _ = s.close_session();
        }
        Err(e) => {
            println!(
                "    OpenSession      {:>6} ms   {e}",
                started.elapsed().as_millis()
            );
            println!();
            println!("    The second session did not open. The host reported the");
            println!("    fault and did not enter an endless loop.");
            return Err(format!("the second OpenSession failed: {e}"));
        }
    }

    Ok(())
}

/// Runs one operation and prints the result.
fn step(
    s: &mut Session,
    name: &'static str,
    code: u16,
    params: &[u32],
) -> Result<session::Outcome, String> {
    let out = s
        .operation(name, code, params)
        .map_err(|e| format!("{e}"))?;

    println!(
        "    {name:<16} {:>6} ms   {:#06x} {}   {} data bytes",
        out.elapsed.as_millis(),
        out.response_code,
        response_name(out.response_code),
        out.data.len()
    );

    if !out.is_ok() {
        return Err(format!(
            "{name} gave {:#06x} {}",
            out.response_code,
            response_name(out.response_code)
        ));
    }
    Ok(out)
}
