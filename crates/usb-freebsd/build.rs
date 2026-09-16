//! Links the native FreeBSD USB library.
//!
//! `libusb20` lives in `/usr/lib/libusb.so.3`, which FreeBSD ships in the base
//! system. The same file also holds the `libusb-1.0` compatibility layer, and
//! this crate does not call that layer. See `docs/00-why.md`.
//!
//! The crate does not run `bindgen` here. `src/sys.rs` holds bindings that
//! `tools/regen-bindings.sh` writes. A user then needs no `libclang` to build.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/sys.rs");

    if cfg!(target_os = "freebsd") {
        println!("cargo:rustc-link-lib=usb");
    }
}
