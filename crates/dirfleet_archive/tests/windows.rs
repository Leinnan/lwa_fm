//! What only Windows does: the Mark of the Web, names Windows can't create,
//! paths longer than its old limit, links without the right to make them,
//! and the formats that are left out there.

#![cfg(windows)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use dirfleet_archive::{ConflictPolicy, CoreError, open_archive, supported_extensions};
use rars::{ArchiveVersion, Builder};

mod common;
use common::*;

fn rar(dir: &Path, name: &str, setup: impl FnOnce(&mut Builder)) -> String {
    let mut builder = Builder::new(ArchiveVersion::Rar50);
    setup(&mut builder);
    let path = dir.join(name);
    builder.write_to_path(&path, None).unwrap();
    path.display().to_string()
}

/// A tar whose member names are written as given, bypassing the checks
/// the tar crate makes on names.
fn tar_with_names(path: &Path, names: &[&str]) {
    let mut builder = tar::Builder::new(fs::File::create(path).unwrap());
    for name in names {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b"hi"[..]).unwrap();
    }
    builder.finish().unwrap();
}

fn zone_stream(path: &Path) -> PathBuf {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    stream.into()
}

#[test]
fn names_windows_refuses_are_changed_not_lost() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("names.tar");
    tar_with_names(
        &path,
        &[
            "ab:c.txt",
            "CON",
            "NUL.txt",
            "x. ",
            "q?*.txt",
            "dir./f",
            "back\\slash",
            "c:drive.txt",
        ],
    );
    let handle = open_archive(path.display().to_string(), None, None).unwrap();
    // A name that starts like a drive is refused on every system.
    assert_eq!(handle.info().unsafe_names, ["c:drive.txt"]);
    let out = tmp.path().join("out");
    let summary = extract(&handle, request(&out)).unwrap();
    assert_eq!(summary.files, 7);
    let expected = [
        "_CON",
        "_NUL.txt",
        "ab_c.txt",
        "back_slash",
        "dir_/",
        "dir_/f",
        "q__.txt",
        "x__",
    ];
    assert_eq!(tree(&out), expected);
    // The list shows the names that extraction creates.
    let mut listed: Vec<String> = handle
        .entries()
        .into_iter()
        .map(|e| if e.is_dir { e.path + "/" } else { e.path })
        .collect();
    listed.sort();
    assert_eq!(listed, expected);
    // `ab:c.txt` must not become a stream of a file named `ab`.
    assert!(!out.join("ab").exists());
}

#[test]
fn mark_of_the_web_reaches_files_and_keeps_their_times() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = rar(tmp.path(), "mixed.rar", |b| {
        b.add_directory(b"dir".to_vec(), Some(1_700_000_000), None)
            .unwrap();
        b.add_bytes(
            b"dir/small.txt".to_vec(),
            b"small".to_vec(),
            Some(1_700_000_000),
            None,
        )
        .unwrap();
        b.add_bytes(
            b"dir/deep/large.bin".to_vec(),
            vec![7; 1 << 20],
            Some(1_700_000_100),
            None,
        )
        .unwrap();
    });
    let handle = open_archive(archive.clone(), None, None).unwrap();

    let plain = tmp.path().join("plain");
    extract(&handle, request(&plain)).unwrap();
    assert!(fs::read(zone_stream(&plain.join("dir/small.txt"))).is_err());

    fs::write(
        zone_stream(Path::new(&archive)),
        "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/mixed.rar\r\n",
    )
    .unwrap();
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    for item in ["dir/small.txt", "dir/deep/large.bin"] {
        assert_eq!(
            fs::read_to_string(zone_stream(&out.join(item))).unwrap(),
            "[ZoneTransfer]\r\nZoneId=3\r\n",
            "{item}"
        );
    }
    for (item, mtime) in [
        ("dir/small.txt", 1_700_000_000),
        ("dir/deep/large.bin", 1_700_000_100),
        ("dir", 1_700_000_000),
    ] {
        let modified = fs::metadata(out.join(item)).unwrap().modified().unwrap();
        let seconds = modified.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(seconds, mtime, "{item}");
    }
    assert_eq!(
        fs::read(out.join("dir/deep/large.bin")).unwrap().len(),
        1 << 20
    );
}

#[test]
fn paths_longer_than_the_old_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = "a-folder-with-a-rather-long-name";
    let name = format!("{}/file.txt", [folder; 10].join("/"));
    assert!(name.len() > 300);
    let archive = rar(tmp.path(), "deep.rar", |b| {
        b.add_bytes(name.clone().into_bytes(), b"deep".to_vec(), None, None)
            .unwrap();
    });
    let handle = open_archive(archive, None, None).unwrap();
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    let file = out.join(name.replace('/', "\\"));
    assert_eq!(fs::read(&file).unwrap(), b"deep");

    assert_eq!(
        handle.find_conflicts(out.display().to_string(), None),
        std::slice::from_ref(&name)
    );
    let mut again = request(&out);
    again.conflict = ConflictPolicy::KeepBoth;
    let summary = extract(&handle, again).unwrap();
    assert_eq!(summary.renamed.len(), 1);
    assert!(file.with_file_name("file 2.txt").exists());
}

#[test]
fn links_become_links_or_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = rar(tmp.path(), "links.rar", |b| {
        // The link comes before what it points at.
        b.add_unix_symlink(
            b"to-file".to_vec(),
            b"docs/a.txt".to_vec(),
            false,
            None,
            None,
        )
        .unwrap();
        b.add_bytes(b"docs/a.txt".to_vec(), b"alpha".to_vec(), None, None)
            .unwrap();
        b.add_unix_symlink(
            b"docs/up".to_vec(),
            b"../docs/a.txt".to_vec(),
            false,
            None,
            None,
        )
        .unwrap();
        b.add_unix_symlink(b"to-dir".to_vec(), b"docs".to_vec(), true, None, None)
            .unwrap();
        b.add_unix_symlink(b"dangling".to_vec(), b"nowhere".to_vec(), false, None, None)
            .unwrap();
        b.add_unix_symlink(b"drive".to_vec(), b"C:secret".to_vec(), false, None, None)
            .unwrap();
        b.add_unix_symlink(b"root".to_vec(), b"\\Windows".to_vec(), false, None, None)
            .unwrap();
    });
    let handle = open_archive(archive, None, None).unwrap();
    let out = tmp.path().join("out");
    let summary = extract(&handle, request(&out)).unwrap();

    // Links to files always arrive: as links, or as copies of the file.
    for link in ["to-file", "docs/up"] {
        assert_eq!(fs::read(out.join(link)).unwrap(), b"alpha", "{link}");
        assert!(!summary.skipped_links.iter().any(|s| s == link), "{link}");
    }
    // Whether this user may make links decides the rest.
    let may_link = out.join("to-file").is_symlink();
    assert_eq!(out.join("to-dir").is_symlink(), may_link);
    assert_eq!(out.join("dangling").is_symlink(), may_link);
    if may_link {
        assert_eq!(
            fs::read_link(out.join("docs/up")).unwrap(),
            PathBuf::from("..\\docs\\a.txt")
        );
        assert_eq!(fs::read(out.join("to-dir/a.txt")).unwrap(), b"alpha");
        assert_eq!(summary.skipped_links, ["drive", "root"]);
    } else {
        assert_eq!(
            summary.skipped_links,
            ["drive", "root", "to-dir", "dangling"]
        );
    }
    // Links that leave the destination are never made.
    assert!(out.join("drive").symlink_metadata().is_err());
    assert!(out.join("root").symlink_metadata().is_err());
}

#[test]
fn a_device_name_is_not_used_for_the_new_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = rar(tmp.path(), "nul.rar", |b| {
        b.add_bytes(b"one.txt".to_vec(), b"1".to_vec(), None, None)
            .unwrap();
        b.add_bytes(b"two.txt".to_vec(), b"2".to_vec(), None, None)
            .unwrap();
    });
    let handle = open_archive(archive, None, None).unwrap();
    let plan = handle.plan_destination(tmp.path().display().to_string());
    assert!(plan.creates_folder);
    assert_eq!(
        plan.output_dir,
        tmp.path().join("_nul").display().to_string()
    );
    let mut request = request(Path::new(&plan.output_dir));
    request.conflict = ConflictPolicy::Skip;
    extract(&handle, request).unwrap();
    assert_eq!(tree(&tmp.path().join("_nul")), ["one.txt", "two.txt"]);
}

#[test]
fn disk_images_and_fallback_formats_are_left_out() {
    let extensions = supported_extensions();
    for missing in ["dmg", "iso", "sparseimage", "lzh", "warc"] {
        assert!(!extensions.iter().any(|e| e == missing), "{missing}");
    }
    for present in ["rar", "zip", "7z", "tar", "cab", "deb"] {
        assert!(extensions.iter().any(|e| e == present), "{present}");
    }

    let tmp = tempfile::tempdir().unwrap();
    let image = tmp.path().join("disc.iso");
    let mut bytes = vec![0u8; 0x8800];
    bytes[0x8001..0x8006].copy_from_slice(b"CD001");
    fs::write(&image, bytes).unwrap();
    assert!(matches!(
        open_archive(image.display().to_string(), None, None),
        Err(CoreError::Unsupported { .. })
    ));
}

/// The formats most archives on Windows come in, built here so the test
/// needs no other program.
#[test]
fn zip_and_compressed_tar_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [
        ("docs/a.txt", "alpha\n".repeat(1000)),
        ("docs/sub/b.txt", "beta".to_string()),
        ("top.txt", "top".to_string()),
    ];

    let zip_path = tmp.path().join("tree.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    writer.add_directory("empty/", options).unwrap();
    for (name, contents) in &files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap();

    let tar_path = tmp.path().join("tree.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        fs::File::create(&tar_path).unwrap(),
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    builder
        .append_data(&mut header, "empty/", std::io::empty())
        .unwrap();
    for (name, contents) in &files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        builder
            .append_data(&mut header, name, contents.as_bytes())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();

    for archive in [&zip_path, &tar_path] {
        let handle = open_archive(archive.display().to_string(), None, None)
            .unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
        let out = archive.with_extension("out");
        extract(&handle, request(&out)).unwrap();
        assert_eq!(
            tree(&out),
            [
                "docs/",
                "docs/a.txt",
                "docs/sub/",
                "docs/sub/b.txt",
                "empty/",
                "top.txt"
            ],
            "{}",
            archive.display()
        );
        for (name, contents) in &files {
            assert_eq!(&fs::read_to_string(out.join(name)).unwrap(), contents);
        }
    }
}
