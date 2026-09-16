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
use usb_freebsd::device::{Backend, OpenDevice};

use session::{response_name, Session};

/// The deadline for one transfer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The buffer size for a bulk endpoint, in bytes.
const BULK_BUFFER: u32 = 16 * 1024;

/// The count of cycles the reopen test does, if the user gives no count.
const DEFAULT_CYCLES: u32 = 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    println!("mtpprobe {}", env!("CARGO_PKG_VERSION"));
    println!("timeout per transfer: {} ms", TIMEOUT.as_millis());
    println!();

    let result = match args.first().map(String::as_str) {
        None | Some("probe") => probe(),
        Some("reopen") => {
            let cycles = args
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(DEFAULT_CYCLES);
            reopen(cycles)
        }
        Some("--help") | Some("-h") => {
            usage();
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            println!("mtpprobe: {other} is not a command.");
            usage();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => {
            println!();
            println!("RESULT: pass.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!();
            println!("RESULT: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    println!("Commands:");
    println!("  mtpprobe probe          Read the device one time. This is the default.");
    println!("  mtpprobe reopen [n]     Open and close the USB device n times.");
    println!();
    println!("The reopen command repeats the pattern that simple-mtpfs uses.");
    println!("See docs/00-why.md.");
}

/// Finds the first device that gives an MTP interface, and opens the device.
fn find_mtp(backend: &Backend, quiet: bool) -> Result<(OpenDevice<'_>, MtpInterface), String> {
    let devices = backend.devices();
    if !quiet {
        println!("The host sees {} USB devices.", devices.len());
    }

    for d in devices {
        let (vid, pid) = d.ids();
        let bus = d.bus();
        let addr = d.address();

        // A device the host cannot open is not a fault. Many devices belong to
        // a kernel driver.
        let mut open = match d.open(4) {
            Ok(o) => o,
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
            if !quiet {
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
            }
            return Ok((open, iface));
        }
    }

    Err(
        "no device gives an MTP interface. Connect the phone, unlock the \
         phone, and choose File transfer on the phone."
            .to_string(),
    )
}

/// Reads the device one time.
fn probe() -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, false)?;

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

    let storage_ids = storage_ids_from(&ids.data);
    if storage_ids.is_empty() {
        report_no_storage();
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

    // `simple-mtpfs` opens a session, closes the session, and opens a second
    // session. A test showed that the phone accepts the second session. See
    // docs/00-why.md.
    println!();
    println!("--- Session 2: a second PTP session ---");
    let o = s
        .open_session(2)
        .map_err(|e| format!("the second OpenSession failed: {e}"))?;
    println!(
        "    OpenSession      {:>6} ms   {:#06x} {}",
        o.elapsed.as_millis(),
        o.response_code,
        response_name(o.response_code)
    );
    let _ = s.close_session();

    println!();
    println!("The device answers over MTP.");
    Ok(())
}

/// Opens and closes the USB device many times.
///
/// This test repeats the pattern that `simple-mtpfs` uses. A USB device open
/// is not the same as a PTP session open. The PTP session cycle already works,
/// so the USB device cycle is the difference that no test covers yet.
fn reopen(cycles: u32) -> Result<(), String> {
    println!("--- USB device open and close cycle ---");
    println!("The test opens the USB device, reads the storage, and closes the");
    println!("device. The test repeats the cycle {cycles} times.");
    println!();
    println!("simple-mtpfs uses this pattern, and simple-mtpfs stops. The PTP");
    println!("session cycle already works. See docs/00-why.md.");
    println!();

    let backend = Backend::new().map_err(|e| e.to_string())?;

    // Check one time that a device is present, and show the device.
    {
        let (_open, _iface) = find_mtp(&backend, false)?;
    }
    println!();

    let mut slowest = Duration::ZERO;
    let mut total = Duration::ZERO;

    for n in 1..=cycles {
        let started = Instant::now();

        // The whole cycle sits in a block, so every handle closes at the end
        // of the block. The close is the part under test.
        let outcome = (|| -> Result<u16, String> {
            let (mut open, iface) = find_mtp(&backend, true)?;
            if open.kernel_driver_active(iface.interface_number) {
                let _ = open.detach_kernel_driver(iface.interface_number);
            }
            let channels = open
                .open_mtp(&iface, BULK_BUFFER)
                .map_err(|e| format!("cannot open the endpoints: {e}"))?;
            let mut s = Session::new(channels, TIMEOUT);

            s.open_session(1).map_err(|e| format!("{e}"))?;
            let ids = s
                .operation("GetStorageIDs", session::OP_GET_STORAGE_IDS, &[])
                .map_err(|e| format!("{e}"))?;
            let count = storage_ids_from(&ids.data).len() as u16;
            s.close_session().map_err(|e| format!("{e}"))?;
            Ok(count)
        })();

        let elapsed = started.elapsed();
        total += elapsed;
        if elapsed > slowest {
            slowest = elapsed;
        }

        match outcome {
            Ok(count) => {
                println!(
                    "  cycle {n:>3}/{cycles}   {:>6} ms   {count} storage(s)",
                    elapsed.as_millis()
                );
            }
            Err(e) => {
                println!(
                    "  cycle {n:>3}/{cycles}   {:>6} ms   FAIL",
                    elapsed.as_millis()
                );
                println!();
                println!("  The cycle failed at cycle {n}.");
                println!("  {e}");
                println!();
                println!("  The host reported the fault and did not enter an");
                println!("  endless loop. A timeout is a result, not a hang.");
                return Err(format!("the reopen cycle failed at cycle {n}"));
            }
        }
    }

    println!();
    println!("  cycles:  {cycles}");
    println!("  slowest: {} ms", slowest.as_millis());
    println!(
        "  mean:    {} ms",
        total.as_millis() / u128::from(cycles.max(1))
    );
    println!();
    println!("Every cycle finished. The USB device open and close cycle does not");
    println!("stop the host when the host uses libusb20.");
    Ok(())
}

/// Reads the storage identifiers from the payload of `GetStorageIDs`.
///
/// The payload starts with a `u32` count, and the identifiers follow.
fn storage_ids_from(data: &[u8]) -> Vec<u32> {
    data.get(4..)
        .unwrap_or(&[])
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Tells the user why a device reports no storage.
fn report_no_storage() {
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
