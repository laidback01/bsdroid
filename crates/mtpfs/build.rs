//! Links the FUSE library.
//!
//! FreeBSD ships `libfuse` version 3 in the port `fusefs-libs3`. The crate
//! does not run `bindgen` here. `src/sys.rs` holds bindings that
//! `tools/regen-fuse-bindings.sh` writes.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/sys.rs");
    println!("cargo:rustc-link-search=native=/usr/local/lib");
    println!("cargo:rustc-link-lib=fuse3");
}
