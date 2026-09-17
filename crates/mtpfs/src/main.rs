//! Mounts an Android device as a folder.
//!
//! The program runs in one thread. MTP holds one session, and a session does
//! one operation at a time, so a second thread waits. See
//! `docs/07-filesystem-design.md`.

use std::ffi::{CStr, CString};
use std::sync::Mutex;

use mtpfs::backend::{Mtp, Settings};
use mtpfs::tree::{Lookup, Tree, ROOT};

#[allow(clippy::all)]
#[path = "sys.rs"]
mod sys;

/// The state the callbacks share.
///
/// FUSE calls a C function pointer, and a C function pointer carries no state.
/// The state therefore lives here.
struct Fs {
    mtp: Mtp,
    tree: Tree,
    /// The files a program is writing, by path.
    ///
    /// MTP needs the size of a file before the bytes. FUSE gives the bytes
    /// with no size in advance. The host therefore keeps the bytes until the
    /// program closes the file, and sends the file then.
    pending: std::collections::BTreeMap<String, Pending>,
}

/// A file a program is writing.
///
/// The bytes go to a spool file on the disk of the host, and not to memory. A
/// copy of a file of 4 GB therefore costs 4 GB of disk, and about 512 KB of
/// memory.
struct Pending {
    /// The folder that holds the new file.
    parent: u32,
    /// The name of the new file.
    name: String,
    /// The spool file that holds the bytes.
    file: std::fs::File,
    /// The path of the spool file.
    spool: std::path::PathBuf,
    /// The count of bytes the program wrote.
    size: u64,
    /// What the send to the device gave, once the send happened.
    ///
    /// `flush` can run more than one time for one open file. The first run
    /// sends the object and keeps the answer here. A later run reads it, and
    /// does not send the object a second time.
    sent: Option<i32>,
    /// The object this write replaces, when the file was already there.
    ///
    /// A new file holds `None`. A file a program opened for writing holds the
    /// handle of the object that was on the cellphone, and the send removes
    /// that object once the new contents arrive.
    replaces: Option<u32>,
    /// The fault a write to the spool file gave, if a write gave one.
    ///
    /// A spool file that took only part of the bytes holds the wrong contents.
    /// The send must not happen, because a part of a file under the right name
    /// is worse than no file. An earlier version sent the part, and a disk
    /// that filled left a short file on the cellphone.
    broken: Option<i32>,
    /// Whether the spool file stays on the disk after this entry goes.
    ///
    /// A failed send normally removes the spool file, because the cellphone
    /// still holds the file the caller opened. A send that had to remove the
    /// old object first has no such copy, so the spool file becomes the only
    /// copy and it stays. See `send_pending`.
    keep_spool: bool,
}

impl Drop for Pending {
    /// Removes the spool file.
    ///
    /// The mount removes the file after a send, and after a fault. A program
    /// that stops the mount in the middle of a write also removes the file,
    /// because `Fs` holds each `Pending`.
    fn drop(&mut self) {
        if self.keep_spool {
            return;
        }
        let _ = std::fs::remove_file(&self.spool);
    }
}

/// Gives the folder for a spool file.
///
/// The order is `BSDROID_SPOOL`, then `TMPDIR`, then `/var/tmp`. `/var/tmp` is
/// on a disk, and `/tmp` on some hosts is in memory. A spool file in memory
/// gives back the cost this design removes.
fn spool_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("BSDROID_SPOOL") {
        return std::path::PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("TMPDIR") {
        return std::path::PathBuf::from(d);
    }
    std::path::PathBuf::from("/var/tmp")
}

// `statvfs` reports the free space of the disk that holds the spool folder.
// The struct comes from the headers of the host, through bindgen, so the
// layout is the layout of this version of FreeBSD.
extern "C" {
    fn statvfs(path: *const i8, buf: *mut sys::statvfs) -> i32;
}

/// Gives the count of free bytes in a folder, for a user who is not root.
///
/// The answer is `None` when the call fails. A caller then goes on, because a
/// missing number is not a reason to refuse the work.
fn free_bytes(dir: &std::path::Path) -> Option<u64> {
    let c = CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: the pointer comes from a CString that lives to the end of the
    // call, and the buffer is one zeroed struct of the right kind.
    let mut buf: sys::statvfs = unsafe { core::mem::zeroed() };
    let rc = unsafe { statvfs(c.as_ptr(), &mut buf) };
    if rc != 0 {
        return None;
    }
    Some(buf.f_bavail * buf.f_frsize)
}

/// The count of spool files this program made.
///
/// The number makes each name different, so two writes at one time do not
/// share a file.
static SPOOL_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Makes a spool file, and gives the file and the path.
fn make_spool() -> std::io::Result<(std::fs::File, std::path::PathBuf)> {
    let n = SPOOL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let path = spool_dir().join(format!("mtpfs-{pid}-{n}.spool"));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    Ok((file, path))
}

/// The state, in a wrapper that the compiler accepts in a static.
///
/// `Fs` holds raw pointers, and a raw pointer is not `Send`. The mount runs in
/// one thread, because `main` puts `-s` in the argument vector, so no callback
/// runs in a second thread.
struct FsCell(Option<Fs>);

// SAFETY: the mount runs in one thread. See the comment above. The option `-s`
// is not a choice a user makes, and `main` adds the option before each
// argument a user gives.
unsafe impl Send for FsCell {}

static FS: Mutex<FsCell> = Mutex::new(FsCell(None));

/// The flags of an `open` call that this program reads.
///
/// FreeBSD sets these values. The project links no `libc`, and `fuse_file_info`
/// gives the flags as a plain `i32`.
mod oflag {
    /// The low two bits hold the access mode.
    pub const ACCMODE: i32 = 0x0003;
    /// The caller writes and does not read.
    pub const WRONLY: i32 = 0x0001;
    /// The caller reads and writes.
    pub const RDWR: i32 = 0x0002;
    /// The caller wants the file empty.
    pub const TRUNC: i32 = 0x0400;

    /// Says whether the caller asked to write.
    pub fn writes(flags: i32) -> bool {
        matches!(flags & ACCMODE, WRONLY | RDWR)
    }
}

/// The bit in `fuse_file_info.fh` that marks a handle a caller can write.
///
/// # Why the mark is needed
///
/// `flush` and `release` run for each close, and a path can be open more than
/// one time. `cp` onto a file that is already there opens the file to write
/// it, and something opens the same file to read it. The close of the reader
/// reached `flush`, which sent a file the writer had not finished, and then
/// `release` removed the spool file. The next write found no spool and
/// answered EROFS.
///
/// A handle for a write carries this bit, and a handle for a read does not.
/// `flush` and `release` now act for the writer alone. An object handle is 32
/// bits wide, and `fh` is 64 bits wide, so the bit is free.
const FH_WRITER: u64 = 1 << 32;

/// Takes the object handle out of a `fh` value.
fn fh_handle(fh: u64) -> u32 {
    (fh & 0xffff_ffff) as u32
}

/// Says whether a `fh` value belongs to a caller that writes.
///
/// A null `fuse_file_info` means the kernel gave no handle. The path then
/// decides, as it did before this bit was there.
unsafe fn fh_writes(fi: *const sys::fuse_file_info) -> bool {
    if fi.is_null() {
        return true;
    }
    unsafe { (*fi).fh & FH_WRITER != 0 }
}

/// The mode bits for a folder that a user can read and enter.
const MODE_DIR: u32 = 0o040_755;
/// The mode bits for a file that a user can read.
const MODE_FILE: u32 = 0o100_644;

/// The count of bytes the host reads at a time when it fills a spool file.
const READ_CHUNK: usize = 1024 * 1024;

/// The largest count of listings one path walk reads.
///
/// A path of many parts needs one listing for each part. The limit stops a
/// walk that does not end. See rule 1 in `docs/00-why.md`.
const MAX_WALK: usize = 64;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let args = &argv[1..];

    let (positional, fuse_args) = split_args(args);

    if args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        return std::process::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-l" || a == "--list") {
        return list_devices();
    }

    // The form follows `mount_msdosfs`: a device, and then a folder.
    //
    //   mtpfs ugen0.11 /mnt/phone
    //   mtpfs /mnt/phone
    //
    // One name is a folder, and the first of two names is a device.
    let (node, mount_point) = match positional.len() {
        1 => (None, positional[0]),
        2 => (Some(positional[0]), positional[1]),
        _ => {
            usage();
            return std::process::ExitCode::FAILURE;
        }
    };

    println!("mtpfs: look for a device");
    // Read every setting one time, and not inside each transfer.
    let settings = Settings::from_env();

    let mtp = match Mtp::open(node, settings) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mtpfs: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("mtpfs: found {}", mtp.name());
    println!(
        "mtpfs: the device supports {} operations",
        mtp.info.operations_supported.len()
    );

    FS.lock().unwrap().0 = Some(Fs {
        mtp,
        tree: Tree::new(),
        pending: std::collections::BTreeMap::new(),
    });

    // SAFETY: every field of the struct is an Option of a function pointer,
    // and a zero is None.
    let mut ops: sys::fuse_operations = unsafe { core::mem::zeroed() };
    ops.getattr = Some(op_getattr);
    ops.readdir = Some(op_readdir);
    ops.open = Some(op_open);
    ops.read = Some(op_read);
    ops.create = Some(op_create);
    ops.write = Some(op_write);
    // `flush` runs on close and the kernel passes its answer to the
    // program. `release` cannot report a fault, so the send lives in
    // `flush`. See the comment on `op_flush`.
    ops.flush = Some(op_flush);
    ops.release = Some(op_release);
    ops.unlink = Some(op_unlink);
    ops.mkdir = Some(op_mkdir);
    ops.rmdir = Some(op_rmdir);
    ops.truncate = Some(op_truncate);
    ops.statfs = Some(op_statfs);
    ops.rename = Some(op_rename);
    // A program that copies a file asks for these three. MTP holds none of
    // them, and a fault here stops the copy. The filesystem therefore accepts
    // each one and changes nothing.
    ops.utimens = Some(op_utimens);
    ops.chmod = Some(op_chmod);
    ops.chown = Some(op_chown);

    // The mount runs in one thread. The option `-s` says so.
    let mut c_args: Vec<CString> = Vec::new();
    c_args.push(CString::new("mtpfs").unwrap());
    c_args.push(CString::new("-s").unwrap());
    for a in &fuse_args {
        c_args.push(CString::new(*a).unwrap());
    }
    c_args.push(CString::new(mount_point).unwrap());

    let mut raw: Vec<*mut i8> = c_args.iter().map(|c| c.as_ptr() as *mut i8).collect();
    raw.push(core::ptr::null_mut());

    let mut version = sys::libfuse_version {
        major: sys::FUSE_MAJOR_VERSION,
        minor: sys::FUSE_MINOR_VERSION,
        hotfix: sys::FUSE_HOTFIX_VERSION,
        padding: 0,
    };

    println!("mtpfs: mount on {mount_point}");
    // SAFETY: the argument vector ends with a null pointer, and the operations
    // struct lives until the call returns.
    let rc = unsafe {
        sys::fuse_main_real_versioned(
            (raw.len() - 1) as i32,
            raw.as_mut_ptr(),
            &ops,
            core::mem::size_of::<sys::fuse_operations>(),
            &mut version,
            core::ptr::null_mut(),
        )
    };

    // Close the session before the program ends.
    FS.lock().unwrap().0 = None;

    if rc == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// Writes one line for each device that gives an MTP interface.
fn list_devices() -> std::process::ExitCode {
    match Mtp::list_devices(Settings::from_env()) {
        Ok(list) if list.is_empty() => {
            println!("No device gives an MTP interface.");
            println!();
            println!("Connect the cellphone, unlock the cellphone, and put the");
            println!("cellphone into file transfer mode.");
            std::process::ExitCode::FAILURE
        }
        Ok(list) => {
            println!("{:<12}  {:<12}  NAME", "NODE", "ID");
            for d in list {
                println!(
                    "{:<12}  {:04x}:{:04x}    {}",
                    d.node, d.vendor_id, d.product_id, d.name
                );
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("mtpfs: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The options that take their value in the next argument.
///
/// FUSE reads the mount options after `-o`. A splitter that does not know
/// this reads `allow_other` as the name of a device, and the mount then fails
/// with a message about a device node.
const OPTIONS_WITH_VALUE: [&str; 1] = ["-o"];

/// Separates what this program reads from what FUSE reads.
///
/// An argument that starts with `-` goes to FUSE. The rest name a device and
/// a folder. An option in [`OPTIONS_WITH_VALUE`] also takes the argument that
/// follows it, so the value does not read as a device name.
///
/// The attached form, such as `-oallow_other`, needs no second argument, and
/// the first rule already covers it.
fn split_args(args: &[String]) -> (Vec<&str>, Vec<&str>) {
    let mut positional: Vec<&str> = Vec::new();
    let mut fuse_args: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with('-') {
            fuse_args.push(a);
            if OPTIONS_WITH_VALUE.contains(&a) {
                // A trailing `-o` with no value is a user fault, and FUSE
                // reports it. The host must not read past the end here.
                if let Some(v) = args.get(i + 1) {
                    fuse_args.push(v.as_str());
                    i += 1;
                }
            }
        } else {
            positional.push(a);
        }
        i += 1;
    }

    (positional, fuse_args)
}

fn usage() {
    println!("mtpfs {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Mounts an Android cellphone as a folder.");
    println!();
    println!("Usage:");
    println!("  mtpfs [device] <mount point> [options]");
    println!("  mtpfs -l");
    println!();
    println!("The device is a node name, such as ugen0.11 or /dev/ugen0.11.");
    println!("With no device, the program takes the first cellphone it finds.");
    println!();
    println!("Examples:");
    println!("  mtpfs /mnt/phone              the first cellphone");
    println!("  mtpfs ugen0.11 /mnt/phone     one named cellphone");
    println!("  mtpfs -l                      each cellphone the host sees");
    println!();
    println!("Before you start:");
    println!("  1. Connect the cellphone.");
    println!("  2. Unlock the cellphone.");
    println!("  3. Put the cellphone into file transfer mode.");
    println!();
    println!("Options go to FUSE:");
    println!("  -f            Stay in the foreground, and write messages.");
    println!("  -d            Stay in the foreground, and write each request.");
    println!("  -o <options>  Give mount options to FUSE.");
    println!();
    println!("To stop:");
    println!("  umount <mount point>");
}

/// Runs a callback body with the shared state.
///
/// FUSE calls a C function pointer, so every callback opens the same way: take
/// the lock, check that the mount still holds a session, and give an error
/// number when it does not. An earlier version wrote those six lines at the
/// top of twelve callbacks.
///
/// `absent` is the error number to give when the mount holds no session.
fn with_fs<F>(absent: i32, body: F) -> i32
where
    F: FnOnce(&mut Fs) -> i32,
{
    let mut guard = match FS.lock() {
        Ok(g) => g,
        // A panic in an earlier callback poisons the lock. The mount is then
        // in an unknown state, so every later call reports a fault.
        Err(_) => return -libc::EIO,
    };
    match guard.0.as_mut() {
        Some(fs) => body(fs),
        None => -absent,
    }
}

/// Finds the handle a path names, and reads a folder when the tree needs one.
///
/// The loop has a count limit, so a path cannot hold the host.
fn resolve(fs: &mut Fs, path: &str) -> Option<u32> {
    for _ in 0..MAX_WALK {
        match fs.tree.lookup(path) {
            Lookup::Found(h) => return Some(h),
            Lookup::NotFound => return None,
            Lookup::NeedListing(parent) => match fs.mtp.list(parent) {
                Ok(entries) => fs.tree.set_children(parent, entries),
                Err(_) => return None,
            },
        }
    }
    None
}

/// Reads a path from C.
fn path_of(p: *const i8) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: FUSE gives a C string that ends with a null.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Gives the attributes of one object.
unsafe extern "C" fn op_getattr(
    path: *const i8,
    st: *mut sys::stat,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    let path = path_of(path);

    with_fs(libc::ENOENT, |fs| {
        // A file a program is writing is not on the device yet.
        if let Some(p) = fs.pending.get(&path) {
            let size = p.size;
            // SAFETY: FUSE gives a buffer for one stat.
            unsafe {
                core::ptr::write_bytes(st, 0, 1);
                (*st).st_mode = MODE_FILE as sys::mode_t;
                (*st).st_nlink = 1;
                (*st).st_size = size as sys::off_t;
                (*st).st_blksize = 65536;
            }
            return 0;
        }

        let handle = match resolve(fs, &path) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };

        // SAFETY: FUSE gives a buffer for one stat.
        unsafe { core::ptr::write_bytes(st, 0, 1) };

        let (mode, size, links) = if handle == ROOT {
            (MODE_DIR, 0u64, 2)
        } else {
            match fs.tree.get(handle) {
                Some(e) if e.is_dir => (MODE_DIR, 0, 2),
                Some(e) => (MODE_FILE, e.size, 1),
                None => return -libc::ENOENT,
            }
        };

        // SAFETY: the pointer is valid, and the fields are plain numbers.
        unsafe {
            (*st).st_mode = mode as sys::mode_t;
            (*st).st_nlink = links;
            (*st).st_size = size as sys::off_t;
            (*st).st_blocks = size.div_ceil(512) as sys::blkcnt_t;
            (*st).st_blksize = 65536;
        }
        0
    })
}

/// Lists a folder.
unsafe extern "C" fn op_readdir(
    path: *const i8,
    buf: *mut core::ffi::c_void,
    filler: sys::fuse_fill_dir_t,
    _offset: sys::off_t,
    _fi: *mut sys::fuse_file_info,
    _flags: sys::fuse_readdir_flags,
) -> i32 {
    let path = path_of(path);

    with_fs(libc::ENOENT, |fs| {
        let handle = match resolve(fs, &path) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };

        // The tree holds the listing after `resolve`, and a folder the walk did
        // not open still needs a read.
        if !fs.tree.is_listed(handle) {
            match fs.mtp.list(handle) {
                Ok(entries) => fs.tree.set_children(handle, entries),
                Err(_) => return -libc::EIO,
            }
        }

        let fill = match filler {
            Some(f) => f,
            None => return -libc::EIO,
        };

        for name in [".", ".."] {
            let c = CString::new(name).unwrap();
            // SAFETY: the filler takes a C string and a null stat.
            unsafe { fill(buf, c.as_ptr(), core::ptr::null(), 0, 0) };
        }

        let entries = match fs.tree.children(handle) {
            Some(e) => e,
            None => return -libc::EIO,
        };

        for e in entries {
            // A name with a null or a separator cannot go in a folder.
            let name = e.name.replace(['/', '\0'], "_");
            let c = match CString::new(name) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // SAFETY: see above.
            unsafe { fill(buf, c.as_ptr(), core::ptr::null(), 0, 0) };
        }
        0
    })
}

/// Opens a file for a read.
unsafe extern "C" fn op_open(path: *const i8, fi: *mut sys::fuse_file_info) -> i32 {
    let path = path_of(path);
    // SAFETY: FUSE gives a valid structure, and `flags` is a plain number.
    let flags = if fi.is_null() {
        0
    } else {
        unsafe { (*fi).flags }
    };

    with_fs(libc::ENOENT, |fs| {
        let handle = match resolve(fs, &path) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };
        let entry = match fs.tree.get(handle) {
            Some(e) if !e.is_dir => e.clone(),
            _ => return -libc::EISDIR,
        };

        // A caller that opens a file to write it needs a spool file. An
        // earlier version made one only in `create`, which FUSE calls for a
        // name the folder does not hold. A caller that opened a file that was
        // already there therefore reached `write` with no spool, and the mount
        // answered EROFS. `cp` onto an existing file failed that way, and so
        // did every text editor.
        if oflag::writes(flags) && !fs.pending.contains_key(&path) {
            // O_TRUNC says the caller wants the file empty, so the old bytes
            // need no read. Any other write opens the file as it is, because
            // the caller can write one byte in the middle and keep the rest.
            let keep = flags & oflag::TRUNC == 0;
            if let Err(rc) = adopt_for_write(fs, &path, handle, &entry, keep) {
                return rc;
            }
        }

        // The mark says what this handle does, and not what another handle
        // does. A handle that reads a file a writer holds open must stay
        // unmarked, or its close sends the file too early.
        let mark = if oflag::writes(flags) { FH_WRITER } else { 0 };
        // SAFETY: FUSE gives a valid structure.
        if !fi.is_null() {
            unsafe { (*fi).fh = u64::from(handle) | mark };
        }
        0
    })
}

/// Makes a spool file for an object that is already on the cellphone.
///
/// `keep` says whether the old bytes go into the spool file. A caller that
/// asked for an empty file needs none of them, and the read costs the whole
/// size of the file.
fn adopt_for_write(
    fs: &mut Fs,
    path: &str,
    handle: u32,
    entry: &mtpfs::tree::Entry,
    keep: bool,
) -> Result<(), i32> {
    // A change to a file needs the whole file on the disk of the host. A
    // refusal now is better than a fault after a read of some gigabytes.
    let dir = spool_dir();
    if keep && entry.size > 0 {
        if let Some(free) = free_bytes(&dir) {
            if entry.size > free {
                eprintln!(
                    "mtpfs: a change to {path} needs {} MB in {:?}, and {} MB is free. \
                     Set BSDROID_SPOOL to a folder with more space.",
                    entry.size / 1_000_000,
                    dir,
                    free / 1_000_000
                );
                return Err(-libc::ENOSPC);
            }
        }
    }

    let (mut file, spool) = match make_spool() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mtpfs: cannot make a spool file in {:?}: {e}", spool_dir());
            return Err(-libc::EIO);
        }
    };

    let mut size = 0u64;
    if keep && entry.size > 0 {
        size = copy_object_to_spool(fs, handle, entry.size, &mut file)?;
    }

    fs.pending.insert(
        path.to_string(),
        Pending {
            parent: entry.parent,
            name: entry.name.clone(),
            file,
            spool,
            size,
            sent: None,
            replaces: Some(handle),
            broken: None,
            keep_spool: false,
        },
    );
    Ok(())
}

/// Reads a whole object from the cellphone into a spool file.
///
/// A caller that opens a file to change part of it needs the other parts. The
/// host holds them on disk, and not in memory, so a change to one byte of a
/// file of 4 GB costs 4 GB of disk and not 4 GB of memory.
fn copy_object_to_spool(
    fs: &mut Fs,
    handle: u32,
    size: u64,
    file: &mut std::fs::File,
) -> Result<u64, i32> {
    use std::io::Write;

    let mut at = 0u64;
    // The loop asks for a count that the device gives, so the bound comes from
    // the size and not from the device. See rule 1 in `docs/00-why.md`.
    while at < size {
        let want = core::cmp::min(READ_CHUNK as u64, size - at) as usize;
        let data = match fs.mtp.read_at(handle, at, want, size) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("mtpfs: cannot read the file to change it: {e}");
                return Err(-libc::EIO);
            }
        };
        if data.is_empty() {
            break;
        }
        if let Err(e) = file.write_all(&data) {
            eprintln!("mtpfs: cannot fill the spool file: {e}");
            return Err(-libc::EIO);
        }
        at += data.len() as u64;
    }
    Ok(at)
}

/// Reports the size and the free space of the storage.
///
/// A program that copies a file asks for the free space first, and a fault
/// here stops the copy. `df` also needs this answer.
unsafe extern "C" fn op_statfs(_path: *const i8, st: *mut sys::statvfs) -> i32 {
    with_fs(libc::EIO, |fs| {
        let (total, free) = match fs.mtp.storage_info() {
            Ok(v) => v,
            Err(_) => return -libc::EIO,
        };

        // A block of 4096 bytes gives a count that fits, for a storage of any
        // size a cellphone holds.
        let block = 4096u64;

        // SAFETY: FUSE gives a buffer for one statvfs.
        unsafe {
            core::ptr::write_bytes(st, 0, 1);
            (*st).f_bsize = block as core::ffi::c_ulong;
            (*st).f_frsize = block as core::ffi::c_ulong;
            (*st).f_blocks = (total / block) as sys::fsblkcnt_t;
            (*st).f_bfree = (free / block) as sys::fsblkcnt_t;
            (*st).f_bavail = (free / block) as sys::fsblkcnt_t;
            (*st).f_namemax = 255;
        }
        0
    })
}

/// Gives an object a new name, and moves the object to another folder.
unsafe extern "C" fn op_rename(from: *const i8, to: *const i8, _flags: u32) -> i32 {
    let from = path_of(from);
    let to = path_of(to);

    with_fs(libc::EIO, |fs| {
        let handle = match resolve(fs, &from) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };
        if handle == ROOT {
            return -libc::EIO;
        }

        let (old_dir, _) = split_parent(&from);
        let (new_dir, new_name) = split_parent(&to);
        if new_name.is_empty() {
            return -libc::EIO;
        }

        let old_parent = match resolve(fs, &old_dir) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };
        let new_parent = match resolve(fs, &new_dir) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };

        // A name that another object already holds must come free first.
        //
        // A cellphone refuses to rename an object onto a name that is taken.
        // Measured on a Samsung SM-S901U: SetObjectPropValue answered 0x2002,
        // which is a general fault, and the mount gave EIO.
        //
        // This is the step a text editor needs. An editor writes the new
        // contents to a file beside the old one, and then renames the new file
        // over the old one. That rename is this call.
        //
        // The old object moves aside, and it goes away only after the new name
        // is in place. A delete of the old object first would destroy the only
        // copy of the old contents before the host knows the rename works.
        let moved_aside = match resolve(fs, &to) {
            // The caller renamed a file onto itself.
            Some(taken) if taken == handle => return 0,
            Some(ROOT) => return -libc::EIO,
            Some(taken) => {
                let aside = aside_name(&new_name);
                if let Err(e) = fs.mtp.rename_object(taken, &aside) {
                    eprintln!("mtpfs: cannot move {to} aside: {e}");
                    return -libc::EIO;
                }
                fs.tree.forget_listing(new_parent);
                Some(taken)
            }
            None => None,
        };

        // A move to another folder comes first, because a name in the new folder
        // must not meet a name in the old one.
        if new_parent != old_parent {
            if !fs.mtp.can_move() {
                return -libc::ENOTSUP;
            }
            if let Err(e) = fs.mtp.move_object(handle, new_parent) {
                eprintln!("mtpfs: cannot move {from}: {e}");
                return -libc::EIO;
            }
        }

        let old_name = fs
            .tree
            .get(handle)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        if old_name != new_name {
            if !fs.mtp.can_rename() {
                return -libc::ENOTSUP;
            }
            if let Err(e) = fs.mtp.rename_object(handle, &new_name) {
                eprintln!("mtpfs: cannot rename {from}: {e}");
                // The old object moved aside for a rename that did not
                // happen. Put it back, so the folder looks as it did.
                if let Some(old) = moved_aside {
                    if let Err(e2) = fs.mtp.rename_object(old, &new_name) {
                        eprintln!(
                            "mtpfs: {to} is now under a name that ends with \
                             .bsdroid-old, and the host cannot put it back: {e2}"
                        );
                    }
                }
                fs.tree.forget_listing(new_parent);
                return -libc::EIO;
            }
        }

        // The new name is in place, so the old object can go.
        if let Some(old) = moved_aside {
            if let Err(e) = fs.mtp.delete_object(old) {
                // The caller got what it asked for. A leftover object is
                // untidy and not a fault of the write.
                eprintln!("mtpfs: {to} is in place, and the old copy remains: {e}");
            }
        }

        fs.tree.forget_listing(old_parent);
        fs.tree.forget_listing(new_parent);
        0
    })
}

/// Accepts a change of time, and changes nothing.
///
/// MTP holds no time that a host can set. A fault here stops `cp -p` and each
/// program that copies a time.
unsafe extern "C" fn op_utimens(
    _path: *const i8,
    _tv: *const sys::timespec,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    0
}

/// Accepts a change of mode, and changes nothing.
///
/// MTP holds no mode. A fault here stops `cp -p`.
unsafe extern "C" fn op_chmod(
    _path: *const i8,
    _mode: sys::mode_t,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    0
}

/// Accepts a change of owner, and changes nothing.
///
/// MTP holds no owner. A fault here stops `cp -p`.
unsafe extern "C" fn op_chown(
    _path: *const i8,
    _uid: sys::uid_t,
    _gid: sys::gid_t,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    0
}

/// Builds a name for an object that moves aside.
///
/// # Why an object moves aside
///
/// MTP has no operation that renames one object onto the name of another. A
/// cellphone refuses that rename, so a replace needs more than one step.
///
/// This code follows the shape of copy on write. ZFS writes new blocks and
/// then moves one pointer, so the old data stays whole until the new data is
/// ready. Here the old object moves to another name, the new contents take
/// the real name, and the old object goes away last.
///
/// MTP gives no atomic step, so the guarantee is smaller than the one ZFS
/// gives:
///
/// - The host never removes the old contents before the new contents hold the
///   real name.
/// - A mount that stops in the middle can leave two objects: the new one under
///   the real name, and the old one under this name. A person sees both, and
///   loses nothing.
fn aside_name(name: &str) -> String {
    let n = SPOOL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{name}.bsdroid-old-{n}")
}

/// Splits a path into the folder and the name.
fn split_parent(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(0) => ("/".to_string(), path[1..].to_string()),
        Some(i) => (path[..i].to_string(), path[i + 1..].to_string()),
        None => ("/".to_string(), path.to_string()),
    }
}

/// Makes a new file, and keeps the bytes until the program closes the file.
unsafe extern "C" fn op_create(
    path: *const i8,
    _mode: sys::mode_t,
    fi: *mut sys::fuse_file_info,
) -> i32 {
    let path = path_of(path);

    with_fs(libc::EIO, |fs| {
        let (dir, name) = split_parent(&path);
        if name.is_empty() {
            return -libc::EIO;
        }
        let parent = match resolve(fs, &dir) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };

        let (file, spool) = match make_spool() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("mtpfs: cannot make a spool file in {:?}: {e}", spool_dir());
                return -libc::EIO;
            }
        };

        fs.pending.insert(
            path,
            Pending {
                parent,
                name,
                file,
                spool,
                size: 0,
                sent: None,
                replaces: None,
                broken: None,
                keep_spool: false,
            },
        );
        if !fi.is_null() {
            unsafe { (*fi).fh = FH_WRITER };
        }
        0
    })
}

/// Keeps the bytes of a write.
unsafe extern "C" fn op_write(
    path: *const i8,
    buf: *const i8,
    size: usize,
    offset: sys::off_t,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    let path = path_of(path);

    with_fs(libc::EIO, |fs| {
        let p = match fs.pending.get_mut(&path) {
            Some(p) => p,
            None => return -libc::EROFS,
        };

        // SAFETY: FUSE gives a buffer of `size` bytes.
        let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, size) };

        use std::os::unix::fs::FileExt;
        let mut done = 0;
        while done < size {
            match p.file.write_at(&bytes[done..], offset as u64 + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) => {
                    // The number from the host says what went wrong. A full
                    // disk gives ENOSPC, and a program that sees ENOSPC can
                    // tell a person what to do. EIO says nothing.
                    let rc = e.raw_os_error().unwrap_or(libc::EIO);
                    eprintln!("mtpfs: cannot write the spool file: {e}");
                    p.broken = Some(rc);
                    return -rc;
                }
            }
        }
        if done < size {
            eprintln!("mtpfs: the spool file took {done} bytes of {size}");
            p.broken = Some(libc::ENOSPC);
            return -libc::ENOSPC;
        }

        let end = offset as u64 + size as u64;
        if end > p.size {
            p.size = end;
        }
        size as i32
    })
}

/// Sends one spooled file to the device.
///
/// The answer is the error number a callback gives back, and 0 for success.
fn send_pending(fs: &mut Fs, p: &mut Pending, path: &str) -> i32 {
    // A spool file that took only part of the bytes must not go to the
    // cellphone. The file the caller opened stays as it was, and the caller
    // already has the fault from the write that failed.
    if let Some(rc) = p.broken {
        eprintln!(
            "mtpfs: {path} does not go to the cellphone, because a write to the spool file failed"
        );
        return -rc;
    }

    // The reader starts at the first byte. `Pending` removes the spool file.
    use std::io::Seek;
    if let Err(e) = p.file.seek(std::io::SeekFrom::Start(0)) {
        eprintln!("mtpfs: cannot read the spool file for {path}: {e}");
        return -libc::EIO;
    }

    let size = p.size;
    let parent = p.parent;
    let name = p.name.clone();

    // A write onto a file that was already there follows the shape of copy on
    // write. See the comment on `aside_name`. The old object moves to another
    // name first, so the cellphone holds the old contents until the new
    // contents arrive under the real name.
    // `false` means the old object went away before the send, so the spool
    // file is the only copy of the contents.
    let mut old_copy_exists = true;
    if let Some(old) = p.replaces {
        let old_size = fs.tree.get(old).map(|e| e.size).unwrap_or(0);
        // An unknown free space means the cellphone gave no answer. The code
        // then tries the safe way, and the cellphone refuses if it must.
        let free = fs.mtp.storage_info().map(|(_, f)| f).unwrap_or(u64::MAX);

        if free >= size {
            // The cellphone holds the old object and the new object at one
            // time, so the old contents stay until the new name is in place.
            let aside = aside_name(&name);
            if let Err(e) = fs.mtp.rename_object(old, &aside) {
                eprintln!("mtpfs: cannot move the old {path} aside: {e}");
                return -libc::EIO;
            }
        } else if free.saturating_add(old_size) >= size {
            // Two copies do not fit. The old object goes first, and the spool
            // file on the host holds the contents until the send ends.
            eprintln!(
                "mtpfs: {path} needs {} MB, and the cellphone has {} MB free. The old copy goes first.",
                size / 1_000_000,
                free / 1_000_000
            );
            if let Err(e) = fs.mtp.delete_object(old) {
                eprintln!("mtpfs: cannot remove the old {path}: {e}");
                return -libc::EIO;
            }
            p.replaces = None;
            old_copy_exists = false;
        } else {
            eprintln!(
                "mtpfs: {path} needs {} MB. The cellphone has {} MB free, and the old copy holds {} MB.",
                size / 1_000_000,
                free / 1_000_000,
                old_size / 1_000_000
            );
            return -libc::ENOSPC;
        }
    }

    match fs.mtp.send_object_stream(parent, &name, &mut p.file, size) {
        Ok(_) => {
            // The new contents hold the real name, so the old object can go.
            if let Some(old) = p.replaces.take() {
                if let Err(e) = fs.mtp.delete_object(old) {
                    // The caller got what it asked for. A leftover object is
                    // untidy, and is not a fault of the write.
                    eprintln!("mtpfs: {path} is in place, and the old copy remains: {e}");
                }
            }
            // The folder holds a new object, so the listing is old.
            fs.tree.forget_listing(parent);
            0
        }
        Err(e) => {
            eprintln!("mtpfs: cannot write {path}: {e}");
            // The new contents did not arrive. Put the old name back, so the
            // caller sees the file it opened.
            if let Some(old) = p.replaces.take() {
                if let Err(e2) = fs.mtp.rename_object(old, &name) {
                    eprintln!(
                        "mtpfs: {path} is now under a name that ends with \
                         .bsdroid-old, and the host cannot put it back: {e2}"
                    );
                }
            }
            if !old_copy_exists {
                // The cellphone had no room for two copies, so the old object
                // went first and the send then failed. The spool file is the
                // only copy, so the spool file stays and its name goes to the
                // log.
                p.keep_spool = true;
                eprintln!(
                    "mtpfs: the cellphone holds no copy of {path}. The contents stay in {:?}.",
                    p.spool
                );
            }
            fs.tree.forget_listing(parent);
            -libc::EIO
        }
    }
}

/// Sends the file to the device when a program closes the file.
///
/// # Why the send happens here, and not in `release`
///
/// The kernel throws away the answer of `release`. The header of `libfuse`
/// says so:
///
/// ```text
/// The return value of release is ignored.
/// ```
///
/// An earlier version of this program sent the whole object in `release`.
/// Every fault on the way to the device therefore went nowhere. A copy that
/// never reached the cellphone reported success, and `cp` said nothing. A
/// cable pulled in the middle of a write showed this: the mount wrote
///
/// ```text
/// mtpfs: cannot write /bsdroid-test/chaos.bin: SendObject: ... error
/// ```
///
/// and `cp` gave the exit code 0 for the same file.
///
/// `flush` runs on `close`, and the kernel does pass its answer to the
/// program. The send belongs here for that reason.
///
/// `flush` can run more than one time for one open file, because `dup` and
/// `fork` both make a second descriptor. The first run sends the object and
/// keeps the answer. A later run gives the same answer and sends nothing.
unsafe extern "C" fn op_flush(path: *const i8, fi: *mut sys::fuse_file_info) -> i32 {
    let path = path_of(path);
    // The close of a handle that reads must not send a file another handle is
    // still writing. See the comment on `FH_WRITER`.
    if !unsafe { fh_writes(fi) } {
        return 0;
    }

    with_fs(libc::EIO, |fs| {
        // Take the entry out, so the send can borrow the session.
        let Some(mut p) = fs.pending.remove(&path) else {
            // A file the mount is not writing. A read needs no flush.
            return 0;
        };

        let rc = match p.sent {
            Some(earlier) => earlier,
            None => {
                let rc = send_pending(fs, &mut p, &path);
                p.sent = Some(rc);
                rc
            }
        };

        fs.pending.insert(path, p);
        rc
    })
}

/// Removes the spool file when a program closes the file.
///
/// `flush` has almost always sent the object by the time this runs. A program
/// that closes a file with no `flush` is rare, and the send happens here for
/// that case. The kernel throws this answer away, so a fault here reaches
/// nobody, which is the whole reason the send moved to `flush`.
unsafe extern "C" fn op_release(path: *const i8, fi: *mut sys::fuse_file_info) -> i32 {
    let path = path_of(path);
    // The close of a handle that reads must not remove the spool file of a
    // handle that writes. See the comment on `FH_WRITER`.
    if !unsafe { fh_writes(fi) } {
        return 0;
    }

    with_fs(0, |fs| {
        let Some(mut p) = fs.pending.remove(&path) else {
            return 0;
        };
        if p.sent.is_none() {
            let _ = send_pending(fs, &mut p, &path);
        }
        // `p` drops here, and the drop removes the spool file.
        0
    })
}

/// Accepts a change of size for a file a program is writing.
unsafe extern "C" fn op_truncate(
    path: *const i8,
    size: sys::off_t,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    let path = path_of(path);

    with_fs(libc::EIO, |fs| {
        match fs.pending.get_mut(&path) {
            Some(p) => {
                if let Err(e) = p.file.set_len(size as u64) {
                    eprintln!("mtpfs: cannot set the size of the spool file: {e}");
                    return -libc::EIO;
                }
                p.size = size as u64;
                0
            }
            None => {
                // No program has the file open for writing. `truncate` on its
                // own reaches here. The file adopts a spool, and the close of
                // the spool sends the shorter file.
                let handle = match resolve(fs, &path) {
                    Some(h) => h,
                    None => return -libc::ENOENT,
                };
                let entry = match fs.tree.get(handle) {
                    Some(e) if !e.is_dir => e.clone(),
                    _ => return -libc::EISDIR,
                };
                // A size of zero needs none of the old bytes.
                let keep = size > 0;
                if let Err(rc) = adopt_for_write(fs, &path, handle, &entry, keep) {
                    return rc;
                }
                let p = fs.pending.get_mut(&path).expect("just inserted");
                if let Err(e) = p.file.set_len(size as u64) {
                    eprintln!("mtpfs: cannot set the size of the spool file: {e}");
                    return -libc::EIO;
                }
                p.size = size as u64;
                // Nothing will call `flush` for this path, because no program
                // holds the file open. The send happens now.
                let mut taken = fs.pending.remove(&path).expect("just inserted");
                send_pending(fs, &mut taken, &path)
            }
        }
    })
}

/// Removes a file.
unsafe extern "C" fn op_unlink(path: *const i8) -> i32 {
    remove(path, false)
}

/// Removes a folder.
unsafe extern "C" fn op_rmdir(path: *const i8) -> i32 {
    remove(path, true)
}

/// Removes an object, and checks the kind first.
fn remove(path: *const i8, want_dir: bool) -> i32 {
    let path = path_of(path);

    with_fs(libc::EIO, |fs| {
        let handle = match resolve(fs, &path) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };
        if handle == ROOT {
            return -libc::EIO;
        }

        let (is_dir, parent) = match fs.tree.get(handle) {
            Some(e) => (e.is_dir, e.parent),
            None => return -libc::ENOENT,
        };
        if is_dir != want_dir {
            return if want_dir {
                -libc::ENOTDIR
            } else {
                -libc::EISDIR
            };
        }

        match fs.mtp.delete_object(handle) {
            Ok(()) => {
                fs.tree.forget_listing(parent);
                0
            }
            Err(e) => {
                eprintln!("mtpfs: cannot remove {path}: {e}");
                -libc::EIO
            }
        }
    })
}

/// Makes a folder.
unsafe extern "C" fn op_mkdir(path: *const i8, _mode: sys::mode_t) -> i32 {
    let path = path_of(path);

    with_fs(libc::EIO, |fs| {
        let (dir, name) = split_parent(&path);
        if name.is_empty() {
            return -libc::EIO;
        }
        let parent = match resolve(fs, &dir) {
            Some(h) => h,
            None => return -libc::ENOENT,
        };

        match fs.mtp.send_object(parent, &name, &[], true) {
            Ok(_) => {
                fs.tree.forget_listing(parent);
                0
            }
            Err(e) => {
                eprintln!("mtpfs: cannot make the folder {path}: {e}");
                -libc::EIO
            }
        }
    })
}

/// Reads part of a file.
unsafe extern "C" fn op_read(
    path: *const i8,
    buf: *mut i8,
    size: usize,
    offset: sys::off_t,
    fi: *mut sys::fuse_file_info,
) -> i32 {
    with_fs(libc::ENOENT, |fs| {
        // A spool file holds bytes the cellphone has not seen. A program that
        // opened a file to read and write it must see what it wrote, so the
        // spool answers and the cellphone does not.
        let p = path_of(path);
        if let Some(pending) = fs.pending.get(&p) {
            let offset = offset as u64;
            if offset >= pending.size {
                return 0;
            }
            let want = core::cmp::min(size as u64, pending.size - offset) as usize;
            let mut out = vec![0u8; want];
            use std::os::unix::fs::FileExt;
            return match pending.file.read_at(&mut out, offset) {
                Ok(n) => {
                    // SAFETY: FUSE gives a buffer of `size` bytes, and `n` is
                    // not larger than `want`.
                    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), buf as *mut u8, n) };
                    n as i32
                }
                Err(e) => {
                    eprintln!("mtpfs: cannot read the spool file: {e}");
                    -libc::EIO
                }
            };
        }

        // The handle comes from `open`, and a path is the fallback.
        let handle = if !fi.is_null() && fh_handle(unsafe { (*fi).fh }) != 0 {
            fh_handle(unsafe { (*fi).fh })
        } else {
            let p = path_of(path);
            match resolve(fs, &p) {
                Some(h) => h,
                None => return -libc::ENOENT,
            }
        };

        let file_size = match fs.tree.get(handle) {
            Some(e) => e.size,
            None => return -libc::ENOENT,
        };

        let offset = offset as u64;
        if offset >= file_size {
            return 0;
        }
        let want = core::cmp::min(size as u64, file_size - offset) as usize;

        // BSDROID_DEBUG reports the size FUSE asks for. The size sets the count of
        // round trips a copy needs, and the count sets the rate.
        //
        // The flag comes from the settings, which the host read one time at
        // startup. An earlier version read the environment here, which is
        // once for every read a copy does.
        if fs.mtp.settings().debug {
            eprintln!("read: offset {offset} size {size} want {want}");
        }

        let data = match fs.mtp.read_at(handle, offset, want, file_size) {
            Ok(d) => d,
            Err(_) => return -libc::EIO,
        };

        let n = core::cmp::min(data.len(), size);
        // SAFETY: FUSE gives a buffer of `size` bytes, and `n` is not larger.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, n) };
        n as i32
    })
}

/// The error numbers the callbacks give.
///
/// FreeBSD sets these values. An earlier version held six functions that each
/// returned a literal, and every call site read `-libc::ENOENT`.
///
/// The module carries the name `libc` so a reader recognises the values, and
/// the project still links no `libc` crate.
mod libc {
    /// No such file or folder.
    pub const ENOENT: i32 = 2;
    /// An input or output fault.
    pub const EIO: i32 = 5;
    /// The name is a folder, and the caller wanted a file.
    pub const EISDIR: i32 = 21;
    /// The name is a file, and the caller wanted a folder.
    pub const ENOTDIR: i32 = 20;
    /// The filesystem is read only.
    pub const EROFS: i32 = 30;
    /// The device cannot do this operation.
    pub const ENOTSUP: i32 = 45;
    /// No space is left on the device.
    pub const ENOSPC: i32 = 28;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(args: &[&str]) -> (Vec<String>, Vec<String>) {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let (p, f) = split_args(&owned);
        (
            p.iter().map(|s| (*s).to_string()).collect(),
            f.iter().map(|s| (*s).to_string()).collect(),
        )
    }

    #[test]
    fn a_folder_alone_is_a_mount_point() {
        let (p, f) = split(&["/mnt/phone"]);
        assert_eq!(p, ["/mnt/phone"]);
        assert!(f.is_empty());
    }

    #[test]
    fn a_device_and_a_folder_both_read_as_names() {
        let (p, f) = split(&["ugen0.11", "/mnt/phone"]);
        assert_eq!(p, ["ugen0.11", "/mnt/phone"]);
        assert!(f.is_empty());
    }

    /// The value of `-o` must not read as the name of a device.
    ///
    /// An earlier version split on the leading `-` alone. The command
    /// `mtpfs -o allow_other /mnt/phone` then gave two names, and the host
    /// reported that `allow_other` is not a device node.
    #[test]
    fn the_value_of_a_mount_option_is_not_a_device_name() {
        let (p, f) = split(&["-o", "allow_other", "/mnt/phone"]);
        assert_eq!(p, ["/mnt/phone"], "the mount point is the only name");
        assert_eq!(f, ["-o", "allow_other"]);
    }

    #[test]
    fn a_mount_option_works_with_a_device_as_well() {
        let (p, f) = split(&["-o", "ro", "ugen0.11", "/mnt/phone"]);
        assert_eq!(p, ["ugen0.11", "/mnt/phone"]);
        assert_eq!(f, ["-o", "ro"]);
    }

    /// `-oallow_other` holds the value in the same argument, so the rule for
    /// a leading `-` already covers it.
    #[test]
    fn an_attached_mount_option_needs_no_second_argument() {
        let (p, f) = split(&["-oallow_other", "/mnt/phone"]);
        assert_eq!(p, ["/mnt/phone"]);
        assert_eq!(f, ["-oallow_other"]);
    }

    #[test]
    fn an_option_that_takes_no_value_keeps_the_next_name() {
        let (p, f) = split(&["-f", "ugen0.11", "/mnt/phone"]);
        assert_eq!(p, ["ugen0.11", "/mnt/phone"]);
        assert_eq!(f, ["-f"]);
    }

    /// A trailing `-o` is a user fault. The splitter must not read past the
    /// end of the argument list.
    #[test]
    fn a_trailing_option_with_no_value_does_not_read_past_the_end() {
        let (p, f) = split(&["/mnt/phone", "-o"]);
        assert_eq!(p, ["/mnt/phone"]);
        assert_eq!(f, ["-o"]);
    }

    #[test]
    fn many_options_all_reach_fuse() {
        let (p, f) = split(&["-f", "-o", "allow_other,ro", "-d", "/mnt/phone"]);
        assert_eq!(p, ["/mnt/phone"]);
        assert_eq!(f, ["-f", "-o", "allow_other,ro", "-d"]);
    }

    #[test]
    fn a_write_flag_says_the_caller_writes() {
        assert!(!oflag::writes(0));
        assert!(oflag::writes(oflag::WRONLY));
        assert!(oflag::writes(oflag::RDWR));
        // O_TRUNC beside O_RDONLY is not a write. The access mode decides.
        assert!(!oflag::writes(oflag::TRUNC));
        assert!(oflag::writes(oflag::WRONLY | oflag::TRUNC));
    }

    #[test]
    fn a_handle_keeps_its_value_beside_the_writer_mark() {
        assert_eq!(fh_handle(u64::from(u32::MAX) | FH_WRITER), u32::MAX);
        assert_eq!(fh_handle(FH_WRITER), 0);
        assert_eq!(fh_handle(42), 42);
        // The mark sits above every value an object handle can hold.
        assert!(FH_WRITER > u64::from(u32::MAX));
    }

    #[test]
    fn a_name_that_moves_aside_is_new_each_time() {
        let first = aside_name("report.txt");
        let second = aside_name("report.txt");
        assert_ne!(first, second);
        assert!(first.starts_with("report.txt.bsdroid-old-"));
        assert!(second.starts_with("report.txt.bsdroid-old-"));
    }
}
