use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use rars::{ArchiveVersion, Builder};

mod common;
use common::*;
use dirfleet_archive::{CancelToken, ConflictPolicy, CoreError, open_archive};

fn build(dir: &Path, name: &str, setup: impl FnOnce(&mut Builder)) -> String {
    let mut builder = Builder::new(ArchiveVersion::Rar50);
    setup(&mut builder);
    let path = dir.join(name);
    builder.write_to_path(&path, None).unwrap();
    path.display().to_string()
}

fn sample(dir: &Path) -> String {
    build(dir, "sample.rar", |b| {
        b.add_directory(b"docs".to_vec(), None, None).unwrap();
        b.add_bytes(b"docs/a.txt".to_vec(), b"alpha".to_vec(), None, None)
            .unwrap();
        b.add_bytes(b"docs/sub/b.txt".to_vec(), b"beta".to_vec(), None, None)
            .unwrap();
        b.add_bytes(b"top.txt".to_vec(), b"top".to_vec(), None, None)
            .unwrap();
    })
}

#[test]
fn lists_tree_with_synthesized_folders_and_rolled_up_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let entries = handle.entries();
    let paths: Vec<_> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "docs",
            "docs/a.txt",
            "docs/sub",
            "docs/sub/b.txt",
            "top.txt"
        ]
    );
    let docs = &entries[0];
    assert!(docs.is_dir && docs.parent_id.is_none());
    assert_eq!(docs.size, 9);
    assert_eq!(entries[3].parent_id, Some(entries[2].id));
    let info = handle.info();
    assert_eq!(info.name, "sample");
    assert_eq!(info.file_count, 3);
    assert_eq!(info.total_size, 12);
}

#[test]
fn extracts_everything_with_byte_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let out = tmp.path().join("out");
    let recorder = Arc::new(Recorder::default());
    let summary = handle
        .extract(request(&out), recorder.clone(), CancelToken::new())
        .unwrap();
    assert_eq!(
        tree(&out),
        [
            "docs/",
            "docs/a.txt",
            "docs/sub/",
            "docs/sub/b.txt",
            "top.txt"
        ]
    );
    assert_eq!(fs::read(out.join("docs/sub/b.txt")).unwrap(), b"beta");
    assert_eq!((summary.files, summary.bytes), (3, 12));
    assert_eq!(recorder.calls.lock().unwrap().last(), Some(&(12, 12)));
    let mut top = summary.top_level_paths.clone();
    top.sort();
    assert_eq!(
        top,
        [
            out.join("docs").display().to_string(),
            out.join("top.txt").display().to_string()
        ]
    );
}

#[test]
fn selecting_a_folder_extracts_its_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let out = tmp.path().join("out");
    let mut req = request(&out);
    req.selection = Some(vec![id_of(&handle, "docs/sub")]);
    extract(&handle, req).unwrap();
    assert_eq!(tree(&out), ["docs/", "docs/sub/", "docs/sub/b.txt"]);
}

#[test]
fn solid_archive_partial_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "solid.rar", |b| {
        *b = Builder::new(ArchiveVersion::Rar50).solid(true);
        for i in 0..3 {
            b.add_bytes(
                format!("f{i}.txt").into_bytes(),
                format!("content {i}").repeat(50).into_bytes(),
                None,
                None,
            )
            .unwrap();
        }
    });
    let handle = open_archive(path, None, None).unwrap();
    assert!(handle.info().is_solid);
    let out = tmp.path().join("out");
    let mut req = request(&out);
    req.selection = Some(vec![id_of(&handle, "f2.txt")]);
    extract(&handle, req).unwrap();
    assert_eq!(tree(&out), ["f2.txt"]);
    assert_eq!(
        fs::read_to_string(out.join("f2.txt")).unwrap(),
        "content 2".repeat(50)
    );
}

#[test]
fn backslash_names_become_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "win.rar", |b| {
        b.add_bytes(b"dir\\file.txt".to_vec(), b"x".to_vec(), None, None)
            .unwrap();
    });
    let handle = open_archive(path, None, None).unwrap();
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(tree(&out), ["dir/", "dir/file.txt"]);
}

#[test]
fn bundled_fixtures_extract() {
    let tmp = tempfile::tempdir().unwrap();
    for name in [
        "multifile.rar",
        "solid.rar",
        "split-store.r01",
        "multivol.part2.rar",
    ] {
        let handle = open_archive(fixture(name), None, None).unwrap();
        let out = tmp.path().join(name);
        let summary = extract(&handle, request(&out)).unwrap();
        assert!(summary.files > 0, "{name}");
        assert_eq!(summary.bytes, handle.info().total_size, "{name}");
    }
    assert_eq!(
        open_archive(fixture("multivol.part3.rar"), None, None)
            .unwrap()
            .info()
            .volumes
            .len(),
        3
    );
    assert_eq!(
        open_archive(fixture("split-store.rar"), None, None)
            .unwrap()
            .info()
            .volumes
            .len(),
        3
    );
}

#[test]
fn missing_last_volume_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    for part in ["multivol.part1.rar", "multivol.part2.rar"] {
        fs::copy(fixture(part), tmp.path().join(part)).unwrap();
    }
    let error = open_archive(
        tmp.path().join("multivol.part1.rar").display().to_string(),
        None,
        None,
    )
    .err()
    .unwrap();
    assert!(matches!(error, CoreError::MissingVolume { name } if name == "multivol.part3.rar"));
}

#[test]
fn encrypted_headers_need_the_password_to_open() {
    let path = fixture("header_encrypted.rar");
    assert!(matches!(
        open_archive(path.clone(), None, None).err(),
        Some(CoreError::PasswordRequired)
    ));
    assert!(matches!(
        open_archive(path.clone(), Some("wrong".into()), None).err(),
        Some(CoreError::BadPassword)
    ));
    let handle = open_archive(path, Some("password".into()), None).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    assert!(extract(&handle, request(tmp.path())).unwrap().files > 0);
}

#[test]
fn encrypted_files_need_the_password_to_extract() {
    let handle = open_archive(fixture("password_aes.rar"), None, None).unwrap();
    assert!(handle.info().has_encrypted_entries);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    assert!(matches!(
        extract(&handle, request(&out)),
        Err(CoreError::PasswordRequired)
    ));
    assert!(!out.exists(), "failed extraction leaves no folder behind");
    let mut req = request(&out);
    req.password = Some("wrong".into());
    assert!(matches!(extract(&handle, req), Err(CoreError::BadPassword)));
    let mut req = request(&out);
    req.password = Some("password".into());
    extract(&handle, req).unwrap();
    assert!(out.join("hello.txt").is_file());
}

#[test]
fn cancel_leaves_nothing_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "big.rar", |b| {
        b.add_bytes(b"small.txt".to_vec(), b"x".to_vec(), None, None)
            .unwrap();
        b.add_bytes(
            b"big.bin".to_vec(),
            (0..8_000_000u32).map(|i| (i * 7 % 251) as u8).collect(),
            None,
            None,
        )
        .unwrap();
    });
    let handle = open_archive(path, None, None).unwrap();
    let out = tmp.path().join("out");
    let token = CancelToken::new();
    let recorder = Arc::new(Recorder {
        cancel_on_first: Some(token.clone()),
        ..Default::default()
    });
    let result = handle.extract(request(&out), recorder, token);
    assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
    assert!(!out.exists());

    // Into an existing folder: the folder stays, but holds nothing new.
    fs::create_dir(&out).unwrap();
    let token = CancelToken::new();
    token.cancel();
    assert!(matches!(
        handle.extract(request(&out), Arc::new(Recorder::default()), token),
        Err(CoreError::Cancelled)
    ));
    assert!(tree(&out).is_empty());
}

#[test]
fn conflict_policies() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(out.join("docs")).unwrap();
    fs::write(out.join("top.txt"), b"mine").unwrap();
    fs::write(out.join("docs/a.txt"), b"mine").unwrap();
    let out_str = out.display().to_string();
    let mut conflicts = handle.find_conflicts(out_str.clone(), None);
    conflicts.sort();
    assert_eq!(conflicts, ["docs/a.txt", "top.txt"]);

    let mut req = request(&out);
    req.conflict = ConflictPolicy::Skip;
    let summary = extract(&handle, req).unwrap();
    assert_eq!(fs::read(out.join("top.txt")).unwrap(), b"mine");
    assert!(out.join("docs/sub/b.txt").is_file());
    assert_eq!(summary.skipped_existing.len(), 2);

    let mut req = request(&out);
    req.conflict = ConflictPolicy::KeepBoth;
    let summary = extract(&handle, req).unwrap();
    assert_eq!(fs::read(out.join("top 2.txt")).unwrap(), b"top");
    assert_eq!(fs::read(out.join("docs/a 2.txt")).unwrap(), b"alpha");
    assert!(summary.renamed.contains(&"top.txt → top 2.txt".to_string()));

    extract(&handle, request(&out)).unwrap();
    assert_eq!(fs::read(out.join("top.txt")).unwrap(), b"top");
    assert!(!tree(&out).iter().any(|p| p.contains(".uunpack-")));
}

#[test]
fn smart_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().display().to_string();

    let multi = open_archive(sample(tmp.path()), None, None).unwrap();
    let plan = multi.plan_destination(parent.clone());
    assert!(plan.creates_folder);
    assert_eq!(
        plan.output_dir,
        tmp.path().join("sample").display().to_string()
    );
    fs::create_dir(tmp.path().join("sample")).unwrap();
    assert_eq!(
        multi.plan_destination(parent.clone()).output_dir,
        tmp.path().join("sample 2").display().to_string()
    );

    let single = open_archive(
        build(tmp.path(), "one.rar", |b| {
            b.add_bytes(b"pack/x.txt".to_vec(), b"x".to_vec(), None, None)
                .unwrap();
        }),
        None,
        None,
    )
    .unwrap();
    let plan = single.plan_destination(parent.clone());
    assert!(!plan.creates_folder);
    assert_eq!(plan.output_dir, parent);
    assert_eq!(
        plan.reveal_path,
        tmp.path().join("pack").display().to_string()
    );
}

#[test]
fn restores_mtime_and_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "meta.rar", |b| {
        b.add_bytes(
            b"run.sh".to_vec(),
            b"#!/bin/sh\n".to_vec(),
            Some(1_600_000_000),
            Some(0o755),
        )
        .unwrap();
    });
    let handle = open_archive(path, None, None).unwrap();
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    let meta = fs::metadata(out.join("run.sh")).unwrap();
    #[cfg(unix)]
    assert_eq!(meta.permissions().mode() & 0o777, 0o755);
    assert_eq!(
        meta.modified().unwrap(),
        UNIX_EPOCH + Duration::from_secs(1_600_000_000)
    );
}

#[test]
fn symlinks_must_stay_inside() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "links.rar", |b| {
        b.add_bytes(b"target.txt".to_vec(), b"t".to_vec(), None, None)
            .unwrap();
        b.add_unix_symlink(b"good".to_vec(), b"target.txt".to_vec(), false, None, None)
            .unwrap();
        b.add_unix_symlink(
            b"evil".to_vec(),
            b"../../etc/passwd".to_vec(),
            false,
            None,
            None,
        )
        .unwrap();
    });
    let handle = open_archive(path, None, None).unwrap();
    let out = tmp.path().join("out");
    let summary = extract(&handle, request(&out)).unwrap();
    match fs::read_link(out.join("good")) {
        Ok(target) => assert_eq!(target, PathBuf::from("target.txt")),
        // Windows only lets some users make links; the others get a copy.
        Err(_) if cfg!(windows) => assert_eq!(fs::read(out.join("good")).unwrap(), b"t"),
        Err(e) => panic!("{e}"),
    }
    assert!(out.join("evil").symlink_metadata().is_err());
    assert_eq!(summary.skipped_links, ["evil"]);
}

/// Writes an archive holding `aa/evil.txt` and `ok.txt`, then rewrites the
/// first name to `../evil.txt` and fixes its header CRC, since the builder
/// refuses unsafe names.
fn traversal_archive(dir: &Path) -> String {
    let path = build(dir, "evil.rar", |b| {
        *b = Builder::new(ArchiveVersion::Rar50).store(true);
        b.add_bytes(b"aa/evil.txt".to_vec(), b"x".to_vec(), None, None)
            .unwrap();
        b.add_bytes(b"ok.txt".to_vec(), b"ok".to_vec(), None, None)
            .unwrap();
    });
    let mut bytes = fs::read(&path).unwrap();
    let at = bytes.windows(11).position(|w| w == b"aa/evil.txt").unwrap();
    let header = (8..at)
        .rev()
        .find_map(|start| {
            let (mut size, mut len) = (0usize, 0);
            while let Some(&b) = bytes.get(start + 4 + len) {
                size |= ((b & 0x7f) as usize) << (7 * len);
                len += 1;
                if b & 0x80 == 0 || len > 3 {
                    break;
                }
            }
            let end = start + 4 + len + size;
            let crc = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
            (end > at && end <= bytes.len() && rars::crc32::crc32(&bytes[start + 4..end]) == crc)
                .then_some((start, end))
        })
        .unwrap();
    bytes[at..at + 11].copy_from_slice(b"../evil.txt");
    let crc = rars::crc32::crc32(&bytes[header.0 + 4..header.1]);
    bytes[header.0..header.0 + 4].copy_from_slice(&crc.to_le_bytes());
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn traversal_names_are_never_extracted() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(traversal_archive(tmp.path()), None, None).unwrap();
    assert_eq!(handle.info().unsafe_names, ["../evil.txt"]);
    assert_eq!(handle.entries().len(), 1);
    let out = tmp.path().join("out");
    let summary = extract(&handle, request(&out)).unwrap();
    assert_eq!(tree(&out), ["ok.txt"]);
    assert!(!tmp.path().join("evil.txt").exists());
    assert_eq!(summary.skipped_unsafe, ["../evil.txt"]);
}

#[test]
fn legacy_code_page_names_are_decoded() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "old.rar", |b| {
        *b = Builder::new(ArchiveVersion::Rar29).store(true);
        // 0x82 is "é" in both CP437 and CP852; 0xA5 is "Ñ" in CP437 and "ą" in CP852.
        b.add_bytes(b"caf\x82 \xa5.txt".to_vec(), b"x".to_vec(), None, None)
            .unwrap();
    });
    let default = open_archive(path.clone(), None, None).unwrap();
    assert_eq!(default.entries()[0].name, "café Ñ.txt");
    let polish = open_archive(path, None, Some("cp852".into())).unwrap();
    assert_eq!(polish.entries()[0].name, "café ą.txt");
    let out = tmp.path().join("out");
    extract(&polish, request(&out)).unwrap();
    assert_eq!(tree(&out), ["café ą.txt"]);
}

#[test]
fn names_differing_only_in_case_are_both_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "case.rar", |b| {
        b.add_bytes(b"Read.txt".to_vec(), b"upper".to_vec(), None, None)
            .unwrap();
        b.add_bytes(b"read.txt".to_vec(), b"lower".to_vec(), None, None)
            .unwrap();
    });
    let handle = open_archive(path, None, None).unwrap();
    let out = tmp.path().join("out");
    let summary = extract(&handle, request(&out)).unwrap();
    let mut contents: Vec<_> = tree(&out)
        .iter()
        .map(|name| fs::read_to_string(out.join(name)).unwrap())
        .collect();
    contents.sort();
    assert_eq!(contents, ["lower", "upper"]);
    // Only a case-insensitive volume (the macOS default) needs a new name.
    let case_insensitive = out.join("READ.TXT").exists();
    assert_eq!(summary.renamed.len(), usize::from(case_insensitive));
}

#[cfg(target_os = "macos")]
#[test]
fn quarantine_is_copied_from_the_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("downloaded.rar");
    fs::copy(fixture("multifile.rar"), &archive).unwrap();
    let value = "0081;5f5e1000;Safari;";
    let status = std::process::Command::new("xattr")
        .args(["-w", "com.apple.quarantine", value])
        .arg(&archive)
        .status()
        .unwrap();
    assert!(status.success());
    let handle = open_archive(archive.display().to_string(), None, None).unwrap();
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    let read = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(out.join("hello.txt"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&read.stdout).trim(), value);
}

/// Small files are written by the writer pool and large ones directly;
/// both, and folders the archive only implies, get the metadata.
#[cfg(target_os = "macos")]
#[test]
fn quarantine_and_metadata_reach_every_item() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = build(tmp.path(), "mixed.rar", |b| {
        b.add_directory(b"dir".to_vec(), Some(1_700_000_000), Some(0o755))
            .unwrap();
        b.add_bytes(
            b"dir/small.txt".to_vec(),
            b"small".to_vec(),
            Some(1_700_000_000),
            Some(0o640),
        )
        .unwrap();
        b.add_bytes(
            b"dir/deep/large.bin".to_vec(),
            vec![7; 1 << 20],
            Some(1_700_000_100),
            Some(0o600),
        )
        .unwrap();
    });
    let value = "0081;5f5e1000;Safari;";
    let status = std::process::Command::new("xattr")
        .args(["-w", "com.apple.quarantine", value, &archive])
        .status()
        .unwrap();
    assert!(status.success());
    let handle = open_archive(archive, None, None).unwrap();
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    for item in ["dir", "dir/deep", "dir/small.txt", "dir/deep/large.bin"] {
        let read = std::process::Command::new("xattr")
            .args(["-p", "com.apple.quarantine"])
            .arg(out.join(item))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&read.stdout).trim(),
            value,
            "{item}"
        );
    }
    for (item, mtime, mode) in [
        ("dir/small.txt", 1_700_000_000, 0o640),
        ("dir/deep/large.bin", 1_700_000_100, 0o600),
        ("dir", 1_700_000_000, 0o755),
    ] {
        let meta = fs::metadata(out.join(item)).unwrap();
        let modified = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap();
        assert_eq!(modified.as_secs(), mtime, "{item}");
        assert_eq!(meta.permissions().mode() & 0o777, mode, "{item}");
    }
    assert_eq!(
        fs::read(out.join("dir/deep/large.bin")).unwrap().len(),
        1 << 20
    );
}

#[test]
fn stale_staging_from_dead_processes_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let out = tmp.path().join("out");
    // No process can have this pid; macOS caps them below 100000.
    let stale = out.join(".uunpack-999999-abc");
    fs::create_dir_all(stale.join("partial")).unwrap();
    extract(&handle, request(&out)).unwrap();
    assert!(!stale.exists());
}

#[test]
fn partial_selection_skips_unwanted_members() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let out = tmp.path().join("out");
    let recorder = Arc::new(Recorder::default());
    let mut req = request(&out);
    req.selection = Some(vec![id_of(&handle, "top.txt")]);
    handle
        .extract(req, recorder.clone(), CancelToken::new())
        .unwrap();
    // Only top.txt (3 bytes) was decoded, not the 9 bytes in docs/.
    assert_eq!(recorder.calls.lock().unwrap().last(), Some(&(3, 3)));
    assert_eq!(handle.selected_size(Some(vec![id_of(&handle, "docs")])), 9);
}

#[test]
fn solid_progress_counts_members_decoded_on_the_way() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "solid.rar", |b| {
        *b = Builder::new(ArchiveVersion::Rar50).solid(true);
        for i in 0..3 {
            b.add_bytes(
                format!("f{i}.txt").into_bytes(),
                vec![b'a' + i as u8; 1000],
                None,
                None,
            )
            .unwrap();
        }
    });
    let handle = open_archive(path, None, None).unwrap();
    let recorder = Arc::new(Recorder::default());
    let mut req = request(&tmp.path().join("out"));
    req.selection = Some(vec![id_of(&handle, "f1.txt")]);
    let summary = handle
        .extract(req, recorder.clone(), CancelToken::new())
        .unwrap();
    // f0 is decoded for history, f2 is never reached.
    assert_eq!(recorder.calls.lock().unwrap().last(), Some(&(2000, 2000)));
    assert_eq!(summary.bytes, 1000);
}

#[test]
fn extract_to_temp_returns_entry_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let handle = open_archive(sample(tmp.path()), None, None).unwrap();
    let temp = tmp.path().join("preview");
    let paths = handle
        .extract_to_temp(
            vec![id_of(&handle, "docs/a.txt")],
            temp.display().to_string(),
            None,
        )
        .unwrap();
    assert_eq!(
        paths,
        [temp.join("docs").join("a.txt").display().to_string()]
    );
    assert_eq!(fs::read(&paths[0]).unwrap(), b"alpha");
    // A second preview replaces the earlier copy.
    handle
        .extract_to_temp(
            vec![id_of(&handle, "docs/a.txt")],
            temp.display().to_string(),
            None,
        )
        .unwrap();
    assert_eq!(tree(&temp), ["docs/", "docs/a.txt"]);
}

#[test]
fn verify_detects_damage() {
    let tmp = tempfile::tempdir().unwrap();
    let path = build(tmp.path(), "check.rar", |b| {
        *b = Builder::new(ArchiveVersion::Rar50).store(true);
        b.add_bytes(
            b"data.txt".to_vec(),
            b"verify me please".to_vec(),
            None,
            None,
        )
        .unwrap();
    });
    let verify = |path: &str| {
        open_archive(path.to_string(), None, None).unwrap().verify(
            None,
            Arc::new(Recorder::default()),
            CancelToken::new(),
        )
    };
    verify(&path).unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let at = bytes.windows(9).position(|w| w == b"verify me").unwrap();
    bytes[at] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    assert!(matches!(verify(&path), Err(CoreError::Corrupt { .. })));
}

/// Solid archives verify by decoding into counting sinks, others through
/// rars' parallel test API; progress reaches the total either way.
#[test]
fn verify_reports_all_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    for solid in [false, true] {
        let path = build(tmp.path(), &format!("verify-{solid}.rar"), |b| {
            *b = Builder::new(ArchiveVersion::Rar50).solid(solid);
            for i in 0..8 {
                b.add_bytes(
                    format!("f{i}.txt").into_bytes(),
                    vec![b'a' + i; 3000],
                    None,
                    None,
                )
                .unwrap();
            }
        });
        let handle = open_archive(path, None, None).unwrap();
        let recorder = Arc::new(Recorder::default());
        handle
            .verify(None, recorder.clone(), CancelToken::new())
            .unwrap();
        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls.last(), Some(&(24_000, 24_000)), "solid: {solid}");
    }
}
