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
const MODE_DIR: u32 = 0o040_555;
/// The mode bits for a file that a user can read.
const MODE_FILE: u32 = 0o100_444;

/// The largest count of listings one path walk reads.
///
/// A path of many parts needs one listing for each part. The limit stops a
/// walk that does not end. See rule 1 in `docs/00-why.md`.
const MAX_WALK: usize = 64;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        println!("mtpfs {}", env!("CARGO_PKG_VERSION"));
        println!();
        println!("Mounts an Android device as a folder.");
        println!();
        println!("Usage:");
        println!("  mtpfs <mount point> [options]");
        println!();
        println!("Before you start:");
        println!("  1. Connect the telephone.");
        println!("  2. Unlock the telephone.");
        println!("  3. Put the telephone into file transfer mode.");
        println!();
        println!("Options:");
        println!("  -f    Stay in the foreground, and write messages.");
        println!("  -d    Stay in the foreground, and write each request.");
        println!();
        println!("To stop:");
        println!("  umount <mount point>");
        return std::process::ExitCode::SUCCESS;
    }

    println!("mtpfs: look for a device");
    let mtp = match Mtp::open() {
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
    });

    // SAFETY: every field of the struct is an Option of a function pointer,
    // and a zero is None.
    let mut ops: sys::fuse_operations = unsafe { core::mem::zeroed() };
    ops.getattr = Some(op_getattr);
    ops.readdir = Some(op_readdir);
    ops.open = Some(op_open);
    ops.read = Some(op_read);

    // The mount runs in one thread. The option `-s` says so.
    let mut argv: Vec<CString> = Vec::new();
    argv.push(CString::new("mtpfs").unwrap());
    argv.push(CString::new("-s").unwrap());
    for a in &args[1..] {
        argv.push(CString::new(a.as_str()).unwrap());
    }
    let mut raw: Vec<*mut i8> = argv.iter().map(|c| c.as_ptr() as *mut i8).collect();
    raw.push(core::ptr::null_mut());

    let mut version = sys::libfuse_version {
        major: sys::FUSE_MAJOR_VERSION,
        minor: sys::FUSE_MINOR_VERSION,
        hotfix: sys::FUSE_HOTFIX_VERSION,
        padding: 0,
    };

    println!("mtpfs: mount");
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

    // The mount is read only in this version.
    // SAFETY: FUSE gives a valid structure.
    if !fi.is_null() {
        let flags = unsafe { (*fi).flags };
        if flags & 0o3 != 0 {
            return -libc_erofs();
        }
        unsafe { (*fi).fh = u64::from(handle) };
    }
    0
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
