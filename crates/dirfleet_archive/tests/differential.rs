//! Extraction checked against libarchive's, through `bsdtar`: for each format
//! both read, the extracted trees must hold the same files with the same
//! bytes and the same link targets. Also covers the libarchive fallback for
//! formats only libarchive reads.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use dirfleet_archive::open_archive;
use rars::{ArchiveVersion, Builder};

mod common;
use common::*;

fn sh(dir: &Path, script: &str) {
    let output = Command::new("sh")
        .args(["-c", script])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// What a tree holds: each file's bytes, each link's target, each folder.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let meta = path.symlink_metadata().unwrap();
            if meta.is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(rel, format!("-> {}", target.display()));
            } else if meta.is_dir() {
                out.insert(format!("{rel}/"), String::new());
                walk(root, &path, out);
            } else {
                let bytes = fs::read(&path).unwrap();
                out.insert(
                    rel,
                    format!("{} bytes, {:x}", bytes.len(), checksum(&bytes)),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn source_tree(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("docs/sub")).unwrap();
    fs::create_dir_all(src.join("empty")).unwrap();
    fs::write(src.join("docs/a.txt"), "alpha\n".repeat(5000)).unwrap();
    fs::write(src.join("docs/sub/b.txt"), "beta").unwrap();
    fs::write(src.join("zero.bin"), "").unwrap();
    let noise: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
        .collect();
    fs::write(src.join("noise.bin"), noise).unwrap();
    std::os::unix::fs::symlink("docs/a.txt", src.join("link")).unwrap();
}

/// Extracts `archive` with uunpack and with bsdtar and compares the trees.
fn compare(archive: &Path) {
    let ours = archive.with_extension("ours");
    let theirs = archive.with_extension("theirs");
    let handle = open_archive(archive.display().to_string(), None, None)
        .unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
    extract(&handle, request(&ours)).unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
    fs::create_dir_all(&theirs).unwrap();
    let status = Command::new("bsdtar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&theirs)
        .status()
        .unwrap();
    assert!(status.success(), "bsdtar {}", archive.display());
    assert_eq!(snapshot(&ours), snapshot(&theirs), "{}", archive.display());
}

#[test]
fn matches_libarchive_on_every_shared_format() {
    let tmp = tempfile::tempdir().unwrap();
    source_tree(tmp.path());
    let src = tmp.path().join("src");
    let mut archives = vec![
        ("info.zip", "zip -qry ../info.zip ."),
        ("t.tar", "bsdtar -cf ../t.tar ."),
        ("t.tar.gz", "bsdtar -czf ../t.tar.gz ."),
        ("t.tar.bz2", "bsdtar -cjf ../t.tar.bz2 ."),
        ("pax.tar", "bsdtar --format pax -cf ../pax.tar ."),
        (
            "odc.cpio",
            "find . | cpio -o -H odc > ../odc.cpio 2>/dev/null",
        ),
        (
            "newc.cpio",
            "find . | cpio -o -H newc > ../newc.cpio 2>/dev/null",
        ),
        ("t.xar", "xar -cf ../t.xar ."),
        (
            "libarchive.7z",
            "bsdtar --format 7zip -cf ../libarchive.7z .",
        ),
        (
            "libarchive.zip",
            "bsdtar --format zip -cf ../libarchive.zip .",
        ),
    ];
    if Command::new("sh")
        .args(["-c", "command -v xz"])
        .status()
        .is_ok_and(|s| s.success())
    {
        archives.push(("t.tar.xz", "bsdtar -cJf ../t.tar.xz ."));
    }
    for (name, script) in &archives {
        sh(&src, script);
        compare(&tmp.path().join(name));
    }

    // RAR from rars, which bsdtar reads too (without links).
    let mut builder = Builder::new(ArchiveVersion::Rar50);
    builder
        .add_bytes(
            b"docs/a.txt".to_vec(),
            "alpha\n".repeat(5000).into_bytes(),
            None,
            None,
        )
        .unwrap();
    builder
        .add_bytes(b"b.txt".to_vec(), b"beta".to_vec(), None, None)
        .unwrap();
    let rar = tmp.path().join("t.rar");
    builder.write_to_path(&rar, None).unwrap();
    compare(&rar);
}

/// WARC has no native backend; libarchive reads it.
#[test]
fn libarchive_reads_what_no_backend_does() {
    let tmp = tempfile::tempdir().unwrap();
    source_tree(tmp.path());
    sh(
        &tmp.path().join("src"),
        "bsdtar --format warc -cf ../t.warc docs",
    );
    let handle = open_archive(tmp.path().join("t.warc").display().to_string(), None, None).unwrap();
    assert!(
        handle.info().format.to_lowercase().contains("warc"),
        "{}",
        handle.info().format
    );
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("docs/sub/b.txt")).unwrap(),
        "beta"
    );
}
