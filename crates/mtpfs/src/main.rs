//! Mounts an Android device as a folder.
//!
//! The program runs in one thread. MTP holds one session, and a session does
//! one operation at a time, so a second thread waits. See
//! `docs/07-filesystem-design.md`.

use std::ffi::{CStr, CString};
use std::sync::Mutex;

use mtpfs::backend::Mtp;
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
struct Pending {
    /// The folder that holds the new file.
    parent: u32,
    /// The name of the new file.
    name: String,
    /// The bytes so far.
    data: Vec<u8>,
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

/// The mode bits for a folder that a user can read and enter.
const MODE_DIR: u32 = 0o040_755;
/// The mode bits for a file that a user can read.
const MODE_FILE: u32 = 0o100_644;

/// The largest count of listings one path walk reads.
///
/// A path of many parts needs one listing for each part. The limit stops a
/// walk that does not end. See rule 1 in `docs/00-why.md`.
const MAX_WALK: usize = 64;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let args = &argv[1..];

    // Separate what this program reads from what FUSE reads. An argument that
    // starts with `-` goes to FUSE, and the rest name a device and a folder.
    let mut positional: Vec<&str> = Vec::new();
    let mut fuse_args: Vec<&str> = Vec::new();
    for a in args {
        if a.starts_with('-') {
            fuse_args.push(a);
        } else {
            positional.push(a);
        }
    }

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
    let mtp = match Mtp::open(node) {
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
    match Mtp::list_devices() {
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
    println!("  -f    Stay in the foreground, and write messages.");
    println!("  -d    Stay in the foreground, and write each request.");
    println!();
    println!("To stop:");
    println!("  umount <mount point>");
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
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_enoent(),
    };

    // A file a program is writing is not on the device yet.
    if let Some(p) = fs.pending.get(&path) {
        let size = p.data.len() as u64;
        // SAFETY: FUSE gives a buffer for one stat.
        unsafe {
            core::ptr::write_bytes(st, 0, 1);
            (*st).st_mode = 0o100_644 as sys::mode_t;
            (*st).st_nlink = 1;
            (*st).st_size = size as sys::off_t;
            (*st).st_blksize = 65536;
        }
        return 0;
    }

    let handle = match resolve(fs, &path) {
        Some(h) => h,
        None => return -libc_enoent(),
    };

    // SAFETY: FUSE gives a buffer for one stat.
    unsafe { core::ptr::write_bytes(st, 0, 1) };

    let (mode, size, links) = if handle == ROOT {
        (MODE_DIR, 0u64, 2)
    } else {
        match fs.tree.get(handle) {
            Some(e) if e.is_dir => (MODE_DIR, 0, 2),
            Some(e) => (MODE_FILE, e.size, 1),
            None => return -libc_enoent(),
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
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_enoent(),
    };

    let handle = match resolve(fs, &path) {
        Some(h) => h,
        None => return -libc_enoent(),
    };

    // The tree holds the listing after `resolve`, and a folder the walk did
    // not open still needs a read.
    if !fs.tree.is_listed(handle) {
        match fs.mtp.list(handle) {
            Ok(entries) => fs.tree.set_children(handle, entries),
            Err(_) => return -libc_eio(),
        }
    }

    let fill = match filler {
        Some(f) => f,
        None => return -libc_eio(),
    };

    for name in [".", ".."] {
        let c = CString::new(name).unwrap();
        // SAFETY: the filler takes a C string and a null stat.
        unsafe { fill(buf, c.as_ptr(), core::ptr::null(), 0, 0) };
    }

    let entries = match fs.tree.children(handle) {
        Some(e) => e,
        None => return -libc_eio(),
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
}

/// Opens a file for a read.
unsafe extern "C" fn op_open(path: *const i8, fi: *mut sys::fuse_file_info) -> i32 {
    let path = path_of(path);
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_enoent(),
    };

    let handle = match resolve(fs, &path) {
        Some(h) => h,
        None => return -libc_enoent(),
    };
    match fs.tree.get(handle) {
        Some(e) if !e.is_dir => {}
        _ => return -libc_eisdir(),
    }

    // SAFETY: FUSE gives a valid structure.
    if !fi.is_null() {
        unsafe { (*fi).fh = u64::from(handle) };
    }
    0
}

/// Reports the size and the free space of the storage.
///
/// A program that copies a file asks for the free space first, and a fault
/// here stops the copy. `df` also needs this answer.
unsafe extern "C" fn op_statfs(_path: *const i8, st: *mut sys::statvfs) -> i32 {
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };

    let (total, free) = match fs.mtp.storage_info() {
        Ok(v) => v,
        Err(_) => return -libc_eio(),
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
}

/// Gives an object a new name, and moves the object to another folder.
unsafe extern "C" fn op_rename(from: *const i8, to: *const i8, _flags: u32) -> i32 {
    let from = path_of(from);
    let to = path_of(to);
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };

    let handle = match resolve(fs, &from) {
        Some(h) => h,
        None => return -libc_enoent(),
    };
    if handle == ROOT {
        return -libc_eio();
    }

    let (old_dir, _) = split_parent(&from);
    let (new_dir, new_name) = split_parent(&to);
    if new_name.is_empty() {
        return -libc_eio();
    }

    let old_parent = match resolve(fs, &old_dir) {
        Some(h) => h,
        None => return -libc_enoent(),
    };
    let new_parent = match resolve(fs, &new_dir) {
        Some(h) => h,
        None => return -libc_enoent(),
    };

    // A move to another folder comes first, because a name in the new folder
    // must not meet a name in the old one.
    if new_parent != old_parent {
        if !fs.mtp.can_move() {
            return -libc_enotsup();
        }
        if let Err(e) = fs.mtp.move_object(handle, new_parent) {
            eprintln!("mtpfs: cannot move {from}: {e}");
            return -libc_eio();
        }
    }

    let old_name = fs
        .tree
        .get(handle)
        .map(|e| e.name.clone())
        .unwrap_or_default();
    if old_name != new_name {
        if !fs.mtp.can_rename() {
            return -libc_enotsup();
        }
        if let Err(e) = fs.mtp.rename_object(handle, &new_name) {
            eprintln!("mtpfs: cannot rename {from}: {e}");
            return -libc_eio();
        }
    }

    fs.tree.forget_listing(old_parent);
    fs.tree.forget_listing(new_parent);
    0
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
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };

    let (dir, name) = split_parent(&path);
    if name.is_empty() {
        return -libc_eio();
    }
    let parent = match resolve(fs, &dir) {
        Some(h) => h,
        None => return -libc_enoent(),
    };

    fs.pending.insert(
        path,
        Pending {
            parent,
            name,
            data: Vec::new(),
        },
    );
    if !fi.is_null() {
        unsafe { (*fi).fh = 0 };
    }
    0
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
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };

    let p = match fs.pending.get_mut(&path) {
        Some(p) => p,
        None => return -libc_erofs(),
    };

    let offset = offset as usize;
    let end = offset + size;
    if p.data.len() < end {
        p.data.resize(end, 0);
    }
    // SAFETY: FUSE gives a buffer of `size` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(buf as *const u8, p.data[offset..].as_mut_ptr(), size)
    };
    size as i32
}

/// Sends the file to the device when a program closes the file.
unsafe extern "C" fn op_release(path: *const i8, _fi: *mut sys::fuse_file_info) -> i32 {
    let path = path_of(path);
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return 0,
    };

    let p = match fs.pending.remove(&path) {
        Some(p) => p,
        None => return 0,
    };

    match fs.mtp.send_object(p.parent, &p.name, &p.data, false) {
        Ok(_) => {
            // The folder holds a new object, so the listing is old.
            fs.tree.forget_listing(p.parent);
            0
        }
        Err(e) => {
            eprintln!("mtpfs: cannot write {path}: {e}");
            -libc_eio()
        }
    }
}

/// Accepts a change of size for a file a program is writing.
unsafe extern "C" fn op_truncate(
    path: *const i8,
    size: sys::off_t,
    _fi: *mut sys::fuse_file_info,
) -> i32 {
    let path = path_of(path);
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };
    match fs.pending.get_mut(&path) {
        Some(p) => {
            p.data.resize(size as usize, 0);
            0
        }
        // A file on the device does not change size in this version.
        None => -libc_erofs(),
    }
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
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };

    let handle = match resolve(fs, &path) {
        Some(h) => h,
        None => return -libc_enoent(),
    };
    if handle == ROOT {
        return -libc_eio();
    }

    let (is_dir, parent) = match fs.tree.get(handle) {
        Some(e) => (e.is_dir, e.parent),
        None => return -libc_enoent(),
    };
    if is_dir != want_dir {
        return if want_dir {
            -libc_enotdir()
        } else {
            -libc_eisdir()
        };
    }

    match fs.mtp.delete_object(handle) {
        Ok(()) => {
            fs.tree.forget_listing(parent);
            0
        }
        Err(e) => {
            eprintln!("mtpfs: cannot remove {path}: {e}");
            -libc_eio()
        }
    }
}

/// Makes a folder.
unsafe extern "C" fn op_mkdir(path: *const i8, _mode: sys::mode_t) -> i32 {
    let path = path_of(path);
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_eio(),
    };

    let (dir, name) = split_parent(&path);
    if name.is_empty() {
        return -libc_eio();
    }
    let parent = match resolve(fs, &dir) {
        Some(h) => h,
        None => return -libc_enoent(),
    };

    match fs.mtp.send_object(parent, &name, &[], true) {
        Ok(_) => {
            fs.tree.forget_listing(parent);
            0
        }
        Err(e) => {
            eprintln!("mtpfs: cannot make the folder {path}: {e}");
            -libc_eio()
        }
    }
}

/// Reads part of a file.
unsafe extern "C" fn op_read(
    path: *const i8,
    buf: *mut i8,
    size: usize,
    offset: sys::off_t,
    fi: *mut sys::fuse_file_info,
) -> i32 {
    let mut guard = FS.lock().unwrap();
    let fs = match guard.0.as_mut() {
        Some(f) => f,
        None => return -libc_enoent(),
    };

    // The handle comes from `open`, and a path is the fallback.
    let handle = if !fi.is_null() && (unsafe { (*fi).fh }) != 0 {
        (unsafe { (*fi).fh }) as u32
    } else {
        let p = path_of(path);
        match resolve(fs, &p) {
            Some(h) => h,
            None => return -libc_enoent(),
        }
    };

    let file_size = match fs.tree.get(handle) {
        Some(e) => e.size,
        None => return -libc_enoent(),
    };

    let offset = offset as u64;
    if offset >= file_size {
        return 0;
    }
    let want = core::cmp::min(size as u64, file_size - offset) as usize;

    // BSDROID_DEBUG reports the size FUSE asks for. The size sets the count of
    // round trips a copy needs, and the count sets the rate.
    if std::env::var("BSDROID_DEBUG").is_ok() {
        eprintln!("read: offset {offset} size {size} want {want}");
    }

    let data = match fs.mtp.read_at(handle, offset, want, file_size) {
        Ok(d) => d,
        Err(_) => return -libc_eio(),
    };

    let n = core::cmp::min(data.len(), size);
    // SAFETY: FUSE gives a buffer of `size` bytes, and `n` is not larger.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, n) };
    n as i32
}

// The error numbers the callbacks give. FreeBSD sets these values.
fn libc_enoent() -> i32 {
    2
}
fn libc_eio() -> i32 {
    5
}
fn libc_eisdir() -> i32 {
    21
}
fn libc_erofs() -> i32 {
    30
}
fn libc_enotdir() -> i32 {
    20
}
fn libc_enotsup() -> i32 {
    45
}
