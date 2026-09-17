//! The map from a path to an MTP object handle.
//!
//! The module is pure. The module does no I/O, so a test runs the module with
//! no device.
//!
//! # Why the module exists
//!
//! MTP gives each object a number, which the standard calls a handle. MTP
//! gives each object a parent handle. A filesystem needs a path, such as
//! `/DCIM/Camera/a.jpg`.
//!
//! This module holds the tree, and answers three questions:
//!
//! 1. What handle does this path name?
//! 2. What does this folder hold?
//! 3. What listing does the caller need next?
//!
//! Question 3 is the important one. The module never reads from a device. The
//! module tells the caller which folder to read, and the caller reads the
//! folder and gives the answer back.

use std::collections::{BTreeMap, BTreeSet};

/// The handle of the root folder.
///
/// MTP gives 0 as the parent of an object in the root. The root itself has no
/// handle, so the module uses 0 for the root.
///
/// A device lists the root with the value 0xffffffff, and not with 0. See
/// `docs/03-object-handles.md`.
pub const ROOT: u32 = 0;

/// One object of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The handle the device gives to the object.
    pub handle: u32,
    /// The handle of the folder that holds the object.
    pub parent: u32,
    /// The name of the object, with no path.
    pub name: String,
    /// True for a folder.
    pub is_dir: bool,
    /// The size of the object, in bytes. A folder gives 0 here.
    pub size: u64,
}

/// What a caller must do to answer a question about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The tree holds the answer, and the answer is this handle.
    Found(u32),
    /// The tree does not hold the answer yet. The caller must read this
    /// folder, give the result to [`Tree::set_children`], and ask again.
    NeedListing(u32),
    /// The path names no object, and no listing changes the answer.
    NotFound,
}

/// The tree of objects, and the listings the host already read.
#[derive(Debug, Default)]
pub struct Tree {
    entries: BTreeMap<u32, Entry>,
    children: BTreeMap<u32, Vec<u32>>,
    listed: BTreeSet<u32>,
}

impl Tree {
    /// Makes an empty tree. The tree holds no listing, not even for the root.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tells you if the host already read a folder.
    pub fn is_listed(&self, parent: u32) -> bool {
        self.listed.contains(&parent)
    }

    /// Records the objects a folder holds.
    ///
    /// The function replaces an earlier listing of the same folder. An object
    /// that the new listing does not hold stays in the tree, because another
    /// path can still name the object.
    pub fn set_children(&mut self, parent: u32, entries: Vec<Entry>) {
        let mut handles = Vec::with_capacity(entries.len());
        for e in entries {
            handles.push(e.handle);
            self.entries.insert(e.handle, e);
        }
        self.children.insert(parent, handles);
        self.listed.insert(parent);
    }

    /// Gives one object.
    pub fn get(&self, handle: u32) -> Option<&Entry> {
        self.entries.get(&handle)
    }

    /// Gives the objects a folder holds.
    ///
    /// The function gives `None` when the host did not read the folder.
    pub fn children(&self, parent: u32) -> Option<Vec<&Entry>> {
        if !self.listed.contains(&parent) {
            return None;
        }
        let handles = self.children.get(&parent)?;
        Some(handles.iter().filter_map(|h| self.entries.get(h)).collect())
    }

    /// Finds the handle a path names.
    ///
    /// The function walks the path from the root. The function stops at the
    /// first part the tree cannot answer, and names the folder the caller must
    /// read.
    pub fn lookup(&self, path: &str) -> Lookup {
        let mut current = ROOT;

        for part in split_path(path) {
            if !self.listed.contains(&current) {
                return Lookup::NeedListing(current);
            }

            let found = self.children_named(current, part);
            match found {
                // A part in the middle of a path should name a folder. The
                // tree does not check that, because a caller that asks the
                // device to list a file gets a fault from the device, and
                // that fault carries more than a guess here would.
                Some(e) => current = e.handle,
                None => return Lookup::NotFound,
            }
        }
        Lookup::Found(current)
    }

    /// Builds the path of an object.
    ///
    /// The function walks from the object to the root. The function gives
    /// `None` when a parent is absent from the tree.
    ///
    /// The loop has a count limit, so a device that gives a cycle of parents
    /// cannot hold the host. See rule 1 in `docs/00-why.md`.
    pub fn path_of(&self, handle: u32) -> Option<String> {
        if handle == ROOT {
            return Some("/".to_string());
        }

        let mut parts: Vec<&str> = Vec::new();
        let mut current = handle;

        for _ in 0..MAX_DEPTH {
            if current == ROOT {
                let mut out = String::new();
                for p in parts.iter().rev() {
                    out.push('/');
                    out.push_str(p);
                }
                return Some(out);
            }
            let e = self.entries.get(&current)?;
            parts.push(&e.name);
            current = e.parent;
        }
        None
    }

    /// Forgets a listing, and keeps the objects.
    ///
    /// A caller uses the function after a write, so the next read of the
    /// folder asks the device again.
    pub fn forget_listing(&mut self, parent: u32) {
        self.listed.remove(&parent);
        self.children.remove(&parent);
    }

    /// Finds a child of a folder by name.
    fn children_named(&self, parent: u32, name: &str) -> Option<&Entry> {
        let handles = self.children.get(&parent)?;
        handles
            .iter()
            .filter_map(|h| self.entries.get(h))
            .find(|e| e.name == name)
    }
}

/// The largest depth the tree walks.
///
/// A device that gives a cycle of parents makes a walk run without end. The
/// limit stops the walk.
const MAX_DEPTH: usize = 256;

/// Splits a path into parts, and drops an empty part.
///
/// The function drops `.` and treats a repeated separator as one separator.
fn split_path(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|p| !p.is_empty() && *p != ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(handle: u32, parent: u32, name: &str) -> Entry {
        Entry {
            handle,
            parent,
            name: name.to_string(),
            is_dir: true,
            size: 0,
        }
    }

    fn file(handle: u32, parent: u32, name: &str, size: u64) -> Entry {
        Entry {
            handle,
            parent,
            name: name.to_string(),
            is_dir: false,
            size,
        }
    }

    /// Builds the tree a Samsung gives, with two levels.
    fn sample() -> Tree {
        let mut t = Tree::new();
        t.set_children(ROOT, vec![dir(1, ROOT, "DCIM"), dir(2, ROOT, "Music")]);
        t.set_children(1, vec![dir(3, 1, "Camera")]);
        t.set_children(3, vec![file(4, 3, "a.jpg", 1000)]);
        t
    }

    #[test]
    fn an_empty_tree_needs_the_root_listing() {
        let t = Tree::new();
        assert_eq!(t.lookup("/"), Lookup::Found(ROOT), "the root needs no read");
        assert_eq!(t.lookup("/DCIM"), Lookup::NeedListing(ROOT));
    }

    #[test]
    fn a_path_of_one_part_resolves_after_one_listing() {
        let mut t = Tree::new();
        t.set_children(ROOT, vec![dir(1, ROOT, "DCIM")]);
        assert_eq!(t.lookup("/DCIM"), Lookup::Found(1));
    }

    #[test]
    fn a_deep_path_names_each_folder_the_caller_must_read() {
        let mut t = Tree::new();
        assert_eq!(t.lookup("/DCIM/Camera/a.jpg"), Lookup::NeedListing(ROOT));

        t.set_children(ROOT, vec![dir(1, ROOT, "DCIM")]);
        assert_eq!(t.lookup("/DCIM/Camera/a.jpg"), Lookup::NeedListing(1));

        t.set_children(1, vec![dir(3, 1, "Camera")]);
        assert_eq!(t.lookup("/DCIM/Camera/a.jpg"), Lookup::NeedListing(3));

        t.set_children(3, vec![file(4, 3, "a.jpg", 1000)]);
        assert_eq!(t.lookup("/DCIM/Camera/a.jpg"), Lookup::Found(4));
    }

    #[test]
    fn a_name_that_is_absent_from_a_read_folder_is_not_found() {
        let t = sample();
        assert_eq!(t.lookup("/DCIM/Nothing"), Lookup::NotFound);
        assert_eq!(t.lookup("/Nothing"), Lookup::NotFound);
    }

    #[test]
    fn a_path_reads_the_same_with_extra_separators() {
        let t = sample();
        for p in [
            "/DCIM/Camera/a.jpg",
            "//DCIM//Camera//a.jpg",
            "/DCIM/./Camera/a.jpg",
            "DCIM/Camera/a.jpg",
        ] {
            assert_eq!(t.lookup(p), Lookup::Found(4), "path {p}");
        }
    }

    #[test]
    fn the_root_reads_as_the_root() {
        let t = sample();
        for p in ["/", "", "//", "/."] {
            assert_eq!(t.lookup(p), Lookup::Found(ROOT), "path {p:?}");
        }
    }

    #[test]
    fn a_path_comes_back_from_a_handle() {
        let t = sample();
        assert_eq!(t.path_of(ROOT).as_deref(), Some("/"));
        assert_eq!(t.path_of(1).as_deref(), Some("/DCIM"));
        assert_eq!(t.path_of(3).as_deref(), Some("/DCIM/Camera"));
        assert_eq!(t.path_of(4).as_deref(), Some("/DCIM/Camera/a.jpg"));
    }

    #[test]
    fn a_path_and_a_lookup_agree() {
        let t = sample();
        for h in [1u32, 2, 3, 4] {
            let p = t.path_of(h).expect("the tree holds the object");
            assert_eq!(t.lookup(&p), Lookup::Found(h), "handle {h} path {p}");
        }
    }

    #[test]
    fn a_handle_the_tree_does_not_hold_gives_no_path() {
        let t = sample();
        assert_eq!(t.path_of(99), None);
    }

    /// A device that gives a cycle of parents must not hold the host.
    #[test]
    fn a_cycle_of_parents_does_not_run_without_end() {
        let mut t = Tree::new();
        // Two objects, and each one names the other as the parent.
        t.set_children(ROOT, vec![]);
        t.entries.insert(10, dir(10, 11, "a"));
        t.entries.insert(11, dir(11, 10, "b"));

        assert_eq!(t.path_of(10), None, "the walk stops and gives no path");
    }

    #[test]
    fn a_folder_the_host_did_not_read_gives_no_children() {
        let t = sample();
        assert!(t.children(2).is_none(), "Music is not read yet");
        assert!(t.children(ROOT).is_some());
    }

    #[test]
    fn a_listing_gives_the_objects_in_order() {
        let t = sample();
        let names: Vec<&str> = t
            .children(ROOT)
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["DCIM", "Music"]);
    }

    #[test]
    fn a_second_listing_replaces_the_first() {
        let mut t = Tree::new();
        t.set_children(ROOT, vec![dir(1, ROOT, "A")]);
        t.set_children(ROOT, vec![dir(2, ROOT, "B")]);

        let names: Vec<&str> = t
            .children(ROOT)
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["B"]);
        assert_eq!(t.lookup("/A"), Lookup::NotFound);
        assert_eq!(t.lookup("/B"), Lookup::Found(2));
    }

    #[test]
    fn a_forgotten_listing_makes_the_caller_read_again() {
        let mut t = sample();
        assert_eq!(t.lookup("/DCIM/Camera"), Lookup::Found(3));

        t.forget_listing(1);
        assert_eq!(t.lookup("/DCIM/Camera"), Lookup::NeedListing(1));
        assert!(t.children(1).is_none());

        // The object stays, so a handle still gives a path.
        assert_eq!(t.path_of(3).as_deref(), Some("/DCIM/Camera"));
    }

    #[test]
    fn a_file_keeps_its_size_and_its_kind() {
        let t = sample();
        let e = t.get(4).expect("the tree holds the file");
        assert_eq!(e.size, 1000);
        assert!(!e.is_dir);

        let d = t.get(1).expect("the tree holds the folder");
        assert!(d.is_dir);
        assert_eq!(d.size, 0);
    }

    /// Two objects of one folder can hold the same name. MTP allows this, and
    /// a filesystem does not. The tree gives the first object, and a later
    /// version gives each object a name that is not the same.
    #[test]
    fn two_objects_with_one_name_give_the_first() {
        let mut t = Tree::new();
        t.set_children(
            ROOT,
            vec![file(1, ROOT, "same.txt", 10), file(2, ROOT, "same.txt", 20)],
        );
        assert_eq!(t.lookup("/same.txt"), Lookup::Found(1));
        assert_eq!(t.children(ROOT).unwrap().len(), 2, "both objects are there");
    }
}
