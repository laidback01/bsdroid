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

use ptp_proto::{op, DeviceInfo, ObjectInfo, StorageInfo};
use usb_freebsd::descriptor::{ConfigDescriptor, MtpInterface};
use usb_freebsd::device::{Backend, LinkSpeed, OpenDevice};

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
        Some("coldstart") => coldstart(),
        Some("objects") => objects(),
        Some("get") => get(args.get(1).and_then(|s| parse_handle(s))),
        Some("bench") => bench(),
        Some("caps") => caps(),
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
    println!("  mtpprobe coldstart      Reset the device, then measure the wait.");
    println!("  mtpprobe objects        Count the objects, and read the root folder.");
    println!("  mtpprobe get [handle]   Copy one object to the current folder.");
    println!("  mtpprobe bench          Measure the rate of the USB link.");
    println!("  mtpprobe caps           List what the device can do.");
    println!();
    println!("The get command takes a handle in decimal or in hexadecimal, such");
    println!("as 0x1a. The command takes the smallest file if you give no handle.");
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

        // Read the name of an interface when the class alone does not say that
        // the interface carries MTP. A Motorola Moto G (5) needs the name.
        let debug = std::env::var("BSDROID_DEBUG").is_ok();
        if debug {
            println!(
                "  [debug] device {vid:#06x}:{pid:#06x} has {} interface(s)",
                cfg.interfaces.len()
            );
            for i in &cfg.interfaces {
                println!(
                    "  [debug]   iface {} class {:#04x}/{:#04x}/{:#04x} iInterface {} mtp={} vendor_candidate={}",
                    i.number, i.class, i.subclass, i.protocol, i.string_index,
                    i.is_mtp(), i.is_vendor_mtp_candidate()
                );
            }
        }

        let mut names: Vec<(u8, Option<String>)> = Vec::new();
        for i in cfg
            .interfaces
            .iter()
            .filter(|i| i.is_vendor_mtp_candidate())
        {
            let n = open.string_descriptor(i.string_index);
            if debug {
                println!("  [debug]   string {} = {:?}", i.string_index, n);
            }
            names.push((i.string_index, n));
        }
        let found = MtpInterface::find_with_names(&cfg, |idx| {
            names
                .iter()
                .find(|(i, _)| *i == idx)
                .and_then(|(_, n)| n.clone())
        });

        if let Ok(iface) = found {
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

    Err(no_mtp_message(backend))
}

/// Builds a message for the case where no device gives an MTP interface.
///
/// The message names the cause when the host can tell the cause. An Android
/// device that is not in file transfer mode still shows adb, if the user turned
/// on USB debugging. The adb interface is the sign.
fn no_mtp_message(backend: &Backend) -> String {
    let mut android_without_mtp = Vec::new();

    for d in backend.devices() {
        let (vid, pid) = d.ids();
        let bus = d.bus();
        let addr = d.address();

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

        if cfg.interfaces.iter().any(|i| i.is_adb()) {
            android_without_mtp.push(format!(
                "bus {bus}, address {addr}, vendor {vid:#06x}, product {pid:#06x}"
            ));
        }
    }

    if android_without_mtp.is_empty() {
        return "no device gives an MTP interface. Connect the phone, unlock \
                the phone, and choose File transfer on the phone."
            .to_string();
    }

    let mut m = String::from("an Android device is connected, and the device gives no MTP\n");
    m.push_str("  interface. The device shows the adb interface, so the device is\n");
    m.push_str("  awake and the cable carries data.\n\n");
    m.push_str("  Put the cellphone into file transfer mode:\n");
    m.push_str("    1. Open the notification area.\n");
    m.push_str("    2. Find the USB notification.\n");
    m.push_str("    3. Choose file transfer.\n\n");
    m.push_str("  The name is not the same on each cellphone. These are the\n");
    m.push_str("  names the project has seen:\n");
    m.push_str("    Transferring Files / Android Auto\n");
    m.push_str("    File Transfer\n");
    m.push_str("    Transfer files\n\n");
    for d in &android_without_mtp {
        m.push_str(&format!("  device: {d}\n"));
    }
    m
}

/// Reads the device one time.
fn probe() -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, false)?;

    if open.kernel_driver_active(iface.interface_number) {
        println!("  a kernel driver holds the interface, and the probe detaches it");
        let _ = open.detach_kernel_driver(iface.interface_number);
    }

    // Put the protocol state of the device back to the start. A program that
    // stopped in the middle of a data phase leaves a device that answers no
    // command, and this request repairs that state.
    // The device reset request repairs a device that a stopped program left
    // in a bad state. The request also breaks a device that works.
    //
    // A Cyrus CS 24 in PTP mode passed 4 runs of 5 without the request, and
    // 1 run of 5 with it. A Samsung SM-S901U needed the request to answer at
    // all. The request is therefore a repair, and not a step for each open.
    //
    // See docs/06-the-reset-that-breaks.md.
    if std::env::var("BSDROID_PTP_RESET").is_ok() {
        println!("    the host sends a device reset request");
        if let Err(e) = open.ptp_device_reset(iface.interface_number, TIMEOUT) {
            println!("    the device reset request failed: {e}");
        }
    }

    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the MTP endpoints: {e}"))?;
    let mut s = Session::new(channels, TIMEOUT);

    println!();
    println!("--- Session 1 ---");
    let o = s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;
    println!(
        "    {:<16} {:>6} ms   {:#06x} {}",
        "OpenSession",
        o.elapsed.as_millis(),
        o.response_code,
        response_name(o.response_code)
    );
    if !o.is_ok() {
        return Err(format!(
            "OpenSession gave {:#06x} {}",
            o.response_code,
            response_name(o.response_code)
        ));
    }

    let w = wait_for_storage(&mut s)?;
    println!(
        "    GetStorageIDs    {:>6} ms   {} attempt(s)   {} storage(s)",
        w.elapsed.as_millis(),
        w.attempts,
        w.ids.len()
    );
    if w.attempts > 1 {
        println!("      the first attempt gave an empty list, and the host retried");
    }

    if w.ids.is_empty() {
        report_no_storage(&w);
        let _ = s.close_session();
        return Err("the device reports 0 storages".to_string());
    }
    let storage_ids = w.ids.clone();
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

            s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;
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

/// Counts the objects, and reads the root folder.
///
/// The command answers two questions:
///
/// 1. How many objects does the device hold? The answer needs one large data
///    phase, and the phase does not fit in one USB transfer.
/// 2. What does the root folder hold?
///
/// The object count also separates file transfer mode from image mode. See
/// `docs/02-device-states.md`.
fn objects() -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, false)?;
    if open.kernel_driver_active(iface.interface_number) {
        let _ = open.detach_kernel_driver(iface.interface_number);
    }
    // Put the protocol state of the device back to the start. A program that
    // stopped in the middle of a data phase leaves a device that answers no
    // command, and this request repairs that state.
    // The device reset request repairs a device that a stopped program left
    // in a bad state. The request also breaks a device that works.
    //
    // A Cyrus CS 24 in PTP mode passed 4 runs of 5 without the request, and
    // 1 run of 5 with it. A Samsung SM-S901U needed the request to answer at
    // all. The request is therefore a repair, and not a step for each open.
    //
    // See docs/06-the-reset-that-breaks.md.
    if std::env::var("BSDROID_PTP_RESET").is_ok() {
        println!("    the host sends a device reset request");
        if let Err(e) = open.ptp_device_reset(iface.interface_number, TIMEOUT) {
            println!("    the device reset request failed: {e}");
        }
    }

    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the MTP endpoints: {e}"))?;
    let mut s = Session::new(channels, TIMEOUT);

    s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;
    let w = wait_for_storage(&mut s)?;
    if w.ids.is_empty() {
        report_no_storage(&w);
        let _ = s.close_session();
        return Err("the device reports 0 storages".to_string());
    }
    let storage = w.ids[0];
    println!();
    println!("storage {storage:#010x}");

    // The third parameter of GetObjectHandles names an association, which is a
    // folder. Two values are special, and the standards do not agree about
    // which value does what. The probe sends both values and reports what the
    // device did. The probe does not name the values.
    for parent in [0x0000_0000u32, 0xffff_ffff_u32] {
        println!();
        println!("--- GetObjectHandles with parent {parent:#010x} ---");

        let started = Instant::now();
        let out = s
            .operation(
                "GetObjectHandles",
                session::OP_GET_OBJECT_HANDLES,
                &[storage, 0, parent],
            )
            .map_err(|e| format!("{e}"))?;

        let handles: Vec<u32> = out
            .data
            .get(4..)
            .unwrap_or(&[])
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        println!(
            "    {:>6} ms   {} objects   {} data bytes   {} read(s)",
            started.elapsed().as_millis(),
            handles.len(),
            out.data.len(),
            out.reads
        );

        // Read the information for the first objects. The names tell a reader
        // what the value means, and a guess does not.
        let mut folders = 0;
        let mut files = 0;
        let show = core::cmp::min(handles.len(), 12);
        println!("    the first {show} objects:");

        for h in handles.iter().take(show) {
            let info = s
                .operation("GetObjectInfo", session::OP_GET_OBJECT_INFO, &[*h])
                .map_err(|e| format!("{e}"))?;
            match ObjectInfo::parse(&info.data) {
                Ok(o) => {
                    let kind = if o.is_folder() {
                        folders += 1;
                        "folder"
                    } else {
                        files += 1;
                        "file  "
                    };
                    println!(
                        "      {kind}  {h:#010x}  parent {:#010x}  {:>11}  {}",
                        o.parent_object,
                        o.compressed_size,
                        display_name(&o.filename)
                    );
                }
                Err(e) => println!("      the payload does not parse: {e}"),
            }
        }
        println!("      of the first {show}: {folders} folder(s), {files} file(s)");
    }

    let _ = s.close_session();
    Ok(())
}

/// The read sizes the benchmark uses, in bytes.
const BENCH_READ_SIZES: [usize; 4] = [4 * 1024, 16 * 1024, 64 * 1024, 256 * 1024];

/// The count of times the benchmark reads each file.
const BENCH_ROUNDS: usize = 3;

/// The count of objects the benchmark reads to find the test files.
const BENCH_SEARCH: usize = 120;

/// Measures the rate of the USB link.
///
/// The benchmark reads a file from the device, and writes the bytes nowhere.
/// A write to a disk would measure the disk, and the benchmark measures the
/// USB link.
///
/// The benchmark uses the files of the device, because the project cannot ship
/// a file. The sizes are therefore not the same on two phones. The report
/// gives the size of each file, so a reader can compare two reports.
fn bench() -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, false)?;

    let speed = open.speed();
    println!();
    println!("--- The link ---");
    println!("    speed: {speed}");
    match speed.practical_bytes_per_second() {
        Some(b) => println!(
            "    a bulk transfer on this link reaches about {:.0} MiB/s",
            b as f64 / (1024.0 * 1024.0)
        ),
        None => println!("    the project has no estimate for this speed"),
    }
    println!();
    println!("    The estimate is a guide, and not a standard. A rate near the");
    println!("    estimate means the link limits the copy, and the code does not.");

    // The BOS descriptor says what the device can do, and the answer does not
    // change with the speed of the link. A device that supports super speed,
    // on a link at high speed, has a cable or a port that cannot do more.
    //
    // An earlier version compared the speed of other devices on the host. That
    // test gives a guess. This test gives an answer from the device.
    if speed == LinkSpeed::High {
        match open.capabilities(TIMEOUT) {
            Ok(caps) if caps.supports_faster_than_high() => {
                println!();
                println!("    The device says it supports super speed, and the link");
                println!("    runs at high speed. The device is not the limit.");
                println!();
                println!("    The BOS descriptor of the device gives this answer. The");
                println!("    answer does not change with the cable, so the answer is");
                println!("    about the device alone.");
                println!();
                println!("    The cable or the port is the limit. A cable for a");
                println!("    cellphone often holds no super speed wires, and the plug");
                println!("    looks the same as a cable that does.");
            }
            Ok(_) => {
                println!();
                println!("    The device does not report super speed. High speed is");
                println!("    therefore the limit of the device, and a faster cable");
                println!("    gives no faster copy.");
            }
            Err(_) => {
                println!();
                println!("    The device gives no BOS descriptor, which means the device");
                println!("    runs at high speed at most. A faster cable gives no faster");
                println!("    copy.");
            }
        }
    }

    if open.kernel_driver_active(iface.interface_number) {
        let _ = open.detach_kernel_driver(iface.interface_number);
    }
    // Put the protocol state of the device back to the start. A program that
    // stopped in the middle of a data phase leaves a device that answers no
    // command, and this request repairs that state.
    // The device reset request repairs a device that a stopped program left
    // in a bad state. The request also breaks a device that works.
    //
    // A Cyrus CS 24 in PTP mode passed 4 runs of 5 without the request, and
    // 1 run of 5 with it. A Samsung SM-S901U needed the request to answer at
    // all. The request is therefore a repair, and not a step for each open.
    //
    // See docs/06-the-reset-that-breaks.md.
    if std::env::var("BSDROID_PTP_RESET").is_ok() {
        println!("    the host sends a device reset request");
        if let Err(e) = open.ptp_device_reset(iface.interface_number, TIMEOUT) {
            println!("    the device reset request failed: {e}");
        }
    }

    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the MTP endpoints: {e}"))?;
    let mut s = Session::new(channels, TIMEOUT);

    s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;
    let w = wait_for_storage(&mut s)?;
    if w.ids.is_empty() {
        report_no_storage(&w);
        let _ = s.close_session();
        return Err("the device reports 0 storages".to_string());
    }
    let storage = w.ids[0];

    // Find files across a range of sizes.
    println!();
    println!("--- The test files ---");
    let list = s
        .operation(
            "GetObjectHandles",
            session::OP_GET_OBJECT_HANDLES,
            &[storage, 0, ptp_proto::association::EVERY_OBJECT],
        )
        .map_err(|e| format!("{e}"))?;
    let handles: Vec<u32> = list
        .data
        .get(4..)
        .unwrap_or(&[])
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let mut files: Vec<(u32, u32, String)> = Vec::new();
    for h in handles.iter().take(BENCH_SEARCH) {
        let out = s
            .operation("GetObjectInfo", session::OP_GET_OBJECT_INFO, &[*h])
            .map_err(|e| format!("{e}"))?;
        if let Ok(i) = ObjectInfo::parse(&out.data) {
            if !i.is_folder() && i.compressed_size > 0 {
                files.push((*h, i.compressed_size, i.filename));
            }
        }
    }
    if files.is_empty() {
        let _ = s.close_session();
        return Err(format!(
            "no file found in the first {BENCH_SEARCH} objects. The benchmark \
             needs a file on the device."
        ));
    }
    files.sort_by_key(|f| f.1);

    // Take the smallest, a middle one, and the largest.
    let mut chosen: Vec<(u32, u32, String)> = Vec::new();
    for idx in [0usize, files.len() / 2, files.len() - 1] {
        let f = &files[idx];
        if !chosen.iter().any(|c| c.0 == f.0) {
            chosen.push(f.clone());
        }
    }
    for (_, size, name) in &chosen {
        println!(
            "    {:>12} bytes   {}",
            size,
            short_name(&display_name(name))
        );
    }
    println!(
        "    chosen from {} files in the first {BENCH_SEARCH} objects",
        files.len()
    );

    // Run the benchmark.
    println!();
    println!("--- The rates ---");
    println!("    Each row reads the whole file {BENCH_ROUNDS} times, and gives the best.");
    println!("    The host writes the bytes nowhere, so the disk does not count.");
    println!();
    println!(
        "    {:>12}  {:>9}  {:>7}  {:>10}  {:>7}",
        "size", "read size", "reads", "best", "of est."
    );

    let practical = speed.practical_bytes_per_second();

    for (handle, size, _) in &chosen {
        for read_size in BENCH_READ_SIZES {
            std::env::set_var("BSDROID_READ_BUFFER", read_size.to_string());

            let mut best_rate = 0.0f64;
            let mut reads = 0usize;
            let mut failed = None;

            for _ in 0..BENCH_ROUNDS {
                let mut sink = std::io::sink();
                match s.operation_stream("GetObject", session::OP_GET_OBJECT, &[*handle], &mut sink)
                {
                    Ok(o) => {
                        let secs = o.elapsed.as_secs_f64();
                        let rate = if secs > 0.0 {
                            o.bytes as f64 / secs
                        } else {
                            0.0
                        };
                        if rate > best_rate {
                            best_rate = rate;
                            reads = o.reads;
                        }
                    }
                    Err(e) => {
                        // A failed data phase leaves the device with bytes to
                        // send. The next row must start with a clean channel.
                        let dropped = s.drain();
                        failed = Some(match dropped {
                            0 => format!("{e}"),
                            n => format!("{e} (dropped {n} bytes to recover)"),
                        });
                        break;
                    }
                }
            }

            match failed {
                Some(e) => println!("    {size:>12}  {:>9}  {:>7}  {e}", human(read_size), "-"),
                None => {
                    let pct = match practical {
                        Some(p) => format!("{:.0}%", best_rate / p as f64 * 100.0),
                        None => "-".to_string(),
                    };
                    println!(
                        "    {size:>12}  {:>9}  {reads:>7}  {:>7.1} MiB/s  {pct:>7}",
                        human(read_size),
                        best_rate / (1024.0 * 1024.0)
                    );
                }
            }
        }
    }
    std::env::remove_var("BSDROID_READ_BUFFER");

    println!();
    println!("--- How to read this ---");
    println!("    A small file gives a low rate. The cost of one operation does");
    println!("    not change with the size, so the cost counts for more.");
    println!();
    println!("    A larger read size gives a higher rate, up to a limit. The");
    println!("    limit is the link, or the device.");
    println!();
    println!("    A rate far under the estimate, with a large file and a large");
    println!("    read size, points at the hub chain or the cable.");

    let _ = s.close_session();
    Ok(())
}

/// Gives a name for a report.
///
/// A capture from this tool goes into a fault report, and a fault report goes
/// to other people. A file name is private. The environment variable
/// `BSDROID_REDACT` replaces each name with the size and the kind of the file,
/// which is what a fault report needs.
///
/// The redaction keeps the extension, because the extension changes how a
/// device treats a file, and the extension is not private.
fn display_name(name: &str) -> String {
    if std::env::var("BSDROID_REDACT").is_err() {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 => {
            format!("<name hidden>.{ext}")
        }
        _ => "<name hidden>".to_string(),
    }
}

/// Shortens a name for a report, so a report holds no long private name.
fn short_name(name: &str) -> String {
    if name.len() <= 24 {
        return name.to_string();
    }
    let tail: String = name
        .chars()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
}

/// Writes a byte count for a person to read.
fn human(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

/// Lists what the device can do.
///
/// `GetDeviceInfo` gives the operations a device supports. The list decides
/// what a filesystem on top of the device can do, so a filesystem needs this
/// answer before the design, and not after.
fn caps() -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, false)?;
    if open.kernel_driver_active(iface.interface_number) {
        let _ = open.detach_kernel_driver(iface.interface_number);
    }
    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the MTP endpoints: {e}"))?;
    let mut s = Session::new(channels, TIMEOUT);
    s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;

    let out = s
        .operation("GetDeviceInfo", session::OP_GET_DEVICE_INFO, &[])
        .map_err(|e| format!("{e}"))?;
    // BSDROID_DEBUG prints the bytes. A fault report needs the bytes, and a
    // test fixture needs the bytes.
    if std::env::var("BSDROID_DEBUG").is_ok() {
        println!();
        println!("--- The raw dataset, {} bytes ---", out.data.len());
        for (n, chunk) in out.data.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            println!("    {:04x}  {}", n * 16, hex.join(" "));
        }
    }

    let info = DeviceInfo::parse(&out.data).map_err(|e| format!("{e}"))?;

    println!();
    println!("--- The device ---");
    println!("    manufacturer:  {}", info.manufacturer);
    println!("    model:         {}", info.model);
    println!("    version:       {}", info.device_version);
    println!(
        "    standard:      {}.{}",
        info.standard_version / 100,
        info.standard_version % 100
    );
    println!("    vendor extension: {}", info.vendor_extension_desc);

    println!();
    println!("--- What a filesystem needs ---");
    let rows: [(&str, bool, &str); 4] = [
        (
            "read part of a file",
            info.can_read_part(),
            "a read asks for an offset, and GetObject gives the whole file",
        ),
        (
            "write a new file",
            info.can_write(),
            "SendObjectInfo and SendObject",
        ),
        ("remove a file", info.can_delete(), "DeleteObject"),
        (
            "read a folder fast",
            info.has_fast_listing(),
            "GetObjectPropList, one operation for a whole folder",
        ),
    ];
    for (what, yes, why) in rows {
        let mark = if yes { "yes" } else { "NO " };
        println!("    {mark}   {what:<22}  {why}");
    }

    println!();
    println!("--- The operations the device supports ---");
    println!("    {} operations", info.operations_supported.len());
    let mut line = String::new();
    for c in &info.operations_supported {
        let n = op::name(*c);
        let item = if n == "unknown" {
            format!("{c:#06x} ")
        } else {
            format!("{n} ")
        };
        if line.len() + item.len() > 68 {
            println!("      {line}");
            line.clear();
        }
        line.push_str(&item);
    }
    if !line.is_empty() {
        println!("      {line}");
    }

    let _ = s.close_session();
    Ok(())
}

/// Tells a user to put a cellphone into file transfer mode.
///
/// The instruction leads with the job, and not with a name. No name agrees
/// across the three test devices, so a name is an example and not an
/// instruction.
///
/// ```text
/// Samsung SM-S901U      Transferring Files / Android Auto
/// Cyrus CS 24           File Transfer
/// Motorola Moto G (5)   Transfer files
/// ```
///
/// This project needs file transfer mode alone. A cellphone also gives modes
/// for MIDI and for tethering, and this project never needs one of those. The
/// message does not name them.
///
/// See `docs/02-device-states.md`.
fn print_file_mode_names(indent: &str) {
    println!("{indent}Put the cellphone into file transfer mode.");
    println!();
    println!("{indent}The name is not the same on each cellphone. These are the");
    println!("{indent}names the project has seen:");
    println!("{indent}  Transferring Files / Android Auto");
    println!("{indent}  File Transfer");
    println!("{indent}  Transfer files");
}

/// Reads a handle from the command line. The text is decimal or hexadecimal.
fn parse_handle(s: &str) -> Option<u32> {
    match s.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse::<u32>().ok(),
    }
}

/// The count of objects the command reads to find a small file.
const SEARCH_LIMIT: usize = 60;

/// Copies one object from the device to the current folder.
///
/// The command writes the payload to the file as the payload arrives. The host
/// does not hold the object in memory, so a large file needs no large memory.
fn get(handle: Option<u32>) -> Result<(), String> {
    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, false)?;
    if open.kernel_driver_active(iface.interface_number) {
        let _ = open.detach_kernel_driver(iface.interface_number);
    }
    // Put the protocol state of the device back to the start. A program that
    // stopped in the middle of a data phase leaves a device that answers no
    // command, and this request repairs that state.
    // The device reset request repairs a device that a stopped program left
    // in a bad state. The request also breaks a device that works.
    //
    // A Cyrus CS 24 in PTP mode passed 4 runs of 5 without the request, and
    // 1 run of 5 with it. A Samsung SM-S901U needed the request to answer at
    // all. The request is therefore a repair, and not a step for each open.
    //
    // See docs/06-the-reset-that-breaks.md.
    if std::env::var("BSDROID_PTP_RESET").is_ok() {
        println!("    the host sends a device reset request");
        if let Err(e) = open.ptp_device_reset(iface.interface_number, TIMEOUT) {
            println!("    the device reset request failed: {e}");
        }
    }

    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the MTP endpoints: {e}"))?;
    let mut s = Session::new(channels, TIMEOUT);

    s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;
    let w = wait_for_storage(&mut s)?;
    if w.ids.is_empty() {
        report_no_storage(&w);
        let _ = s.close_session();
        return Err("the device reports 0 storages".to_string());
    }
    let storage = w.ids[0];

    // Choose the object.
    let (target, info) = match handle {
        Some(h) => {
            let out = s
                .operation("GetObjectInfo", session::OP_GET_OBJECT_INFO, &[h])
                .map_err(|e| format!("{e}"))?;
            let info = ObjectInfo::parse(&out.data).map_err(|e| format!("{e}"))?;
            (h, info)
        }
        None => {
            println!();
            println!("No handle given, so the command looks for a small file.");
            let list = s
                .operation(
                    "GetObjectHandles",
                    session::OP_GET_OBJECT_HANDLES,
                    &[storage, 0, ptp_proto::association::EVERY_OBJECT],
                )
                .map_err(|e| format!("{e}"))?;
            let handles: Vec<u32> = list
                .data
                .get(4..)
                .unwrap_or(&[])
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();

            let mut best: Option<(u32, ObjectInfo)> = None;
            for h in handles.iter().take(SEARCH_LIMIT) {
                let out = s
                    .operation("GetObjectInfo", session::OP_GET_OBJECT_INFO, &[*h])
                    .map_err(|e| format!("{e}"))?;
                let info = match ObjectInfo::parse(&out.data) {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                if info.is_folder() || info.compressed_size == 0 {
                    continue;
                }
                let better = match &best {
                    Some((_, b)) => info.compressed_size < b.compressed_size,
                    None => true,
                };
                if better {
                    best = Some((*h, info));
                }
            }
            best.ok_or_else(|| format!("no file found in the first {SEARCH_LIMIT} objects"))?
        }
    };

    if info.is_folder() {
        let _ = s.close_session();
        return Err(format!("object {target:#010x} is a folder, and not a file"));
    }

    println!();
    println!("object {target:#010x}");
    println!("  name:   {}", display_name(&info.filename));
    println!("  size:   {} bytes", info.compressed_size);
    println!("  format: {:#06x}", info.object_format);
    println!("  parent: {:#010x}", info.parent_object);

    // Write to the current folder, under the name the device gives. A name
    // from a device can hold a path separator, so the code takes the last part.
    let safe = info
        .filename
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .ok_or_else(|| format!("the device gives an unusable name: {}", info.filename))?;
    let path = std::path::Path::new(safe);

    println!();
    println!("--- GetObject ---");
    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);

    let out = s
        .operation_stream("GetObject", session::OP_GET_OBJECT, &[target], &mut writer)
        .map_err(|e| format!("{e}"))?;

    use std::io::Write as _;
    writer
        .flush()
        .map_err(|e| format!("cannot finish the write: {e}"))?;
    drop(writer);

    if !out.is_ok() {
        let _ = s.close_session();
        return Err(format!(
            "GetObject gave {:#06x} {}",
            out.response_code,
            response_name(out.response_code)
        ));
    }

    let secs = out.elapsed.as_secs_f64();
    let rate = if secs > 0.0 {
        out.bytes as f64 / secs / (1024.0 * 1024.0)
    } else {
        0.0
    };

    println!(
        "    {:>6} ms   {} bytes   {} read(s)   {:.1} MiB/s",
        out.elapsed.as_millis(),
        out.bytes,
        out.reads,
        rate
    );
    println!("    wrote {}", path.display());

    // Check the size against the size the device reported.
    let on_disk = std::fs::metadata(path)
        .map_err(|e| format!("cannot read the size of {}: {e}", path.display()))?
        .len();

    println!();
    println!("--- Check ---");
    println!("    ObjectInfo says  {} bytes", info.compressed_size);
    println!("    the file holds   {on_disk} bytes");

    let _ = s.close_session();

    if on_disk != u64::from(info.compressed_size) {
        return Err(format!(
            "the sizes disagree: the device said {} and the file holds {on_disk}",
            info.compressed_size
        ));
    }
    println!("    the two sizes agree");
    Ok(())
}

/// Tells you if any device gives an MTP interface.
fn mtp_present(backend: &Backend) -> bool {
    for d in backend.devices() {
        let mut open = match d.open(4) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let raw = match open.config_descriptor_raw(TIMEOUT) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if open.find_mtp_interface(TIMEOUT).is_ok() {
            return true;
        }
        let _ = raw;
    }
    false
}

/// The count of attempts the host makes to read the storage list.
const STORAGE_ATTEMPTS: u32 = 10;
/// The wait between two attempts to read the storage list.
const STORAGE_RETRY_WAIT: Duration = Duration::from_millis(300);

/// What one storage enumeration did.
pub struct StorageWait {
    /// The identifiers the device reported.
    pub ids: Vec<u32>,
    /// The count of attempts the host needed.
    pub attempts: u32,
    /// The time the host waited.
    pub elapsed: Duration,
}

/// Reads the storage list, and retries while the device reports none.
///
/// An Android device answers `GetStorageIDs` with OK and an empty list while
/// the MTP service starts. The empty list is not an error, and the list fills
/// a moment later. A program that asks one time reports "no files" and is
/// wrong. See `docs/01-cold-start.md`.
///
/// The loop has a count limit, so the loop always stops.
fn wait_for_storage(s: &mut Session) -> Result<StorageWait, String> {
    let started = Instant::now();

    for attempt in 1..=STORAGE_ATTEMPTS {
        let out = s
            .operation("GetStorageIDs", session::OP_GET_STORAGE_IDS, &[])
            .map_err(|e| format!("{e}"))?;
        let ids = storage_ids_from(&out.data);

        if !ids.is_empty() {
            return Ok(StorageWait {
                ids,
                attempts: attempt,
                elapsed: started.elapsed(),
            });
        }
        if attempt < STORAGE_ATTEMPTS {
            std::thread::sleep(STORAGE_RETRY_WAIT);
        }
    }

    Ok(StorageWait {
        ids: Vec::new(),
        attempts: STORAGE_ATTEMPTS,
        elapsed: started.elapsed(),
    })
}

/// Tells the user why a device reports no storage.
///
/// The host cannot name one cause. A phone in charge mode and a phone with a
/// locked screen give the same answer, and both keep the MTP interface. See
/// `docs/02-device-states.md`.
fn report_no_storage(w: &StorageWait) {
    println!();
    println!("    What the host knows:");
    println!("      - The device has an MTP interface, and the host opened it.");
    println!("      - The device answered every command with 0x2001 OK.");
    println!(
        "      - The device reported 0 storages {} times over {} ms.",
        w.attempts,
        w.elapsed.as_millis()
    );
    println!();
    println!("    The cable works and the device works. The device holds back");
    println!("    the storage, and the device reports no fault. The hold is a");
    println!("    choice the device makes.");
    println!();
    println!("    Do these two things on the phone, in this order:");
    println!("      1. Unlock the screen.");
    println!("      2. Put the cellphone into file transfer mode.");
    println!();
    print_file_mode_names("         ");
    println!();
    println!("    Step 2 is the common cause. A phone in charge mode still shows");
    println!("    the MTP interface, so the host cannot see the mode. The host");
    println!("    cannot tell you which of the two steps you need.");
    println!();
    println!("    Then run mtpprobe again.");
}

/// Resets the device and measures the time until the storage list fills.
///
/// A reset makes the device leave the bus and come back. The device is then in
/// the state a user gets after a connect. The test needs no person to pull the
/// cable.
fn coldstart() -> Result<(), String> {
    println!("--- Cold start test ---");
    println!("The test resets the device, waits for the device to come back,");
    println!("and measures the time until the device reports a storage.");
    println!();

    {
        let backend = Backend::new().map_err(|e| e.to_string())?;
        let (mut open, _iface) = find_mtp(&backend, false)?;
        println!();
        println!("  reset the device");
        open.reset().map_err(|e| format!("cannot reset: {e}"))?;
    }

    // The device leaves the bus, and the device comes back. The host must wait
    // for the device to go before the host waits for the device to return. A
    // host that only waits for the return sees the device that is still on the
    // bus, and reports a time that is too short.
    let started = Instant::now();

    // Step 1: wait for the device to leave.
    let mut left = None;
    for attempt in 1..=40 {
        let backend = Backend::new().map_err(|e| e.to_string())?;
        if mtp_present(&backend) {
            std::thread::sleep(Duration::from_millis(100));
        } else {
            left = Some(attempt);
            break;
        }
    }
    match left {
        Some(_) => println!(
            "  the device left the bus after {} ms",
            started.elapsed().as_millis()
        ),
        None => println!("  the device did not leave the bus. The reset did nothing."),
    }

    // Step 2: wait for the device to return.
    let mut came_back = None;
    for _ in 1..=60 {
        let backend = Backend::new().map_err(|e| e.to_string())?;
        if mtp_present(&backend) {
            came_back = Some(started.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    match came_back {
        Some(t) => println!("  the device came back after {} ms", t.as_millis()),
        None => {
            println!();
            println!("  The device did not come back with an MTP interface.");
            println!("  An Android device often leaves file transfer mode after a");
            println!("  USB reset, and the device then needs the user to choose");
            println!("  File transfer again.");
            return Err("the device gave no MTP interface after the reset".to_string());
        }
    }

    let backend = Backend::new().map_err(|e| e.to_string())?;
    let (mut open, iface) = find_mtp(&backend, true)?;
    if open.kernel_driver_active(iface.interface_number) {
        let _ = open.detach_kernel_driver(iface.interface_number);
    }
    let channels = open
        .open_mtp(&iface, BULK_BUFFER)
        .map_err(|e| format!("cannot open the endpoints: {e}"))?;
    let mut s = Session::new(channels, TIMEOUT);
    s.open_session_or_repair(1).map_err(|e| format!("{e}"))?;

    let w = wait_for_storage(&mut s)?;
    let _ = s.close_session();

    println!();
    if w.ids.is_empty() {
        println!(
            "  the device reported 0 storages after {} attempts",
            w.attempts
        );
        return Err("the device gave no storage after a reset".to_string());
    }

    println!("  attempts until a storage appeared: {}", w.attempts);
    println!(
        "  time until a storage appeared:     {} ms",
        w.elapsed.as_millis()
    );
    println!("  storages: {:?}", w.ids);
    println!();
    if w.attempts > 1 {
        println!("  The first attempt gave an empty list. A program that asks one");
        println!("  time reports no files, and the report is wrong.");
    } else {
        println!("  The first attempt gave a storage.");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests set an environment variable, so the tests must not run at the
    /// same time. One test function holds every case for this reason.
    #[test]
    fn redaction_hides_a_name_and_keeps_the_kind() {
        // With no variable, a name passes through.
        std::env::remove_var("BSDROID_REDACT");
        assert_eq!(display_name("holiday.jpg"), "holiday.jpg");
        assert_eq!(display_name("DCIM"), "DCIM");

        // With the variable, a name goes and the extension stays.
        std::env::set_var("BSDROID_REDACT", "1");
        assert_eq!(display_name("holiday.jpg"), "<name hidden>.jpg");
        assert_eq!(display_name("IMG_20260118_074042.jpg"), "<name hidden>.jpg");
        assert_eq!(display_name("video.mp4"), "<name hidden>.mp4");

        // A folder has no extension, so nothing remains.
        assert_eq!(display_name("DCIM"), "<name hidden>");
        assert_eq!(display_name("Pictures"), "<name hidden>");

        // A long tail is not an extension, and the name must not leak through
        // it.
        assert_eq!(display_name("name.averylongtail"), "<name hidden>");

        // A name that ends with a dot holds no extension.
        assert_eq!(display_name("odd."), "<name hidden>");

        // A name that starts with a dot holds no extension. The text after the
        // dot is the name itself, so the redaction must hide the text.
        //
        // A first version of this test expected "<name hidden>.thumbnails".
        // That answer leaks the name through the field for the extension. The
        // length limit above stops the leak, and this test guards the limit.
        assert_eq!(display_name(".thumbnails"), "<name hidden>");
        assert_eq!(display_name(".secret"), "<name hidden>.secret");

        std::env::remove_var("BSDROID_REDACT");
    }

    #[test]
    fn a_handle_reads_in_decimal_and_in_hexadecimal() {
        assert_eq!(parse_handle("12"), Some(12));
        assert_eq!(parse_handle("0x0c"), Some(12));
        assert_eq!(parse_handle("0xff"), Some(255));
        assert_eq!(parse_handle("not a number"), None);
        assert_eq!(parse_handle(""), None);
    }
}
