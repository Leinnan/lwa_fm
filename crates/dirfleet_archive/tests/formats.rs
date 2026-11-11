//! Formats other than RAR, on archives made by the tools that make them in
//! the wild: Info-ZIP, ditto, bsdtar, gzip, bzip2, xz, zstd, lz4, brotli and
//! compress. 7z archives come from sevenz-rust2, as 7-Zip isn't part of macOS.

// Most of these tests run Unix tools; the helpers are unused without them.
#![cfg_attr(not(unix), allow(dead_code, unused_imports, unused_variables))]

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use dirfleet_archive::{CancelToken, CoreError, open_archive};

mod common;
use common::*;

/// Runs `script` with `sh` in `dir`; the tools must exist.
fn sh(dir: &Path, script: &str) {
    let status = Command::new("sh")
        .args(["-c", script])
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "{script}");
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A small tree with a nested folder, an executable, an empty folder and,
/// with `links`, a symlink.
fn source_tree(root: &Path, links: bool) -> std::path::PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("docs/sub")).unwrap();
    fs::create_dir_all(src.join("empty")).unwrap();
    fs::write(src.join("docs/a.txt"), "alpha\n".repeat(1000)).unwrap();
    fs::write(src.join("docs/sub/b.txt"), "beta").unwrap();
    fs::write(src.join("top.txt"), "top").unwrap();
    fs::write(src.join("run.sh"), "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
    #[cfg(unix)]
    if links {
        std::os::unix::fs::symlink("top.txt", src.join("link")).unwrap();
    }
    src
}

const FILES: [&str; 8] = [
    "docs/",
    "docs/a.txt",
    "docs/sub/",
    "docs/sub/b.txt",
    "empty/",
    "link",
    "run.sh",
    "top.txt",
];

/// Extracts `archive` into a fresh folder and checks the tree matches
/// `source_tree`, with or without the symlink.
fn assert_round_trip(archive: &Path, password: Option<&str>, links: bool) {
    let handle = open_archive(
        archive.display().to_string(),
        password.map(str::to_string),
        None,
    )
    .unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
    let out = archive.with_extension("out");
    let _ = fs::remove_dir_all(&out);
    extract(&handle, request(&out)).unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
    let expected: Vec<&str> = FILES
        .iter()
        .copied()
        .filter(|f| links || *f != "link")
        .collect();
    assert_eq!(tree(&out), expected, "{}", archive.display());
    assert_eq!(
        fs::read_to_string(out.join("docs/a.txt")).unwrap(),
        "alpha\n".repeat(1000)
    );
    #[cfg(unix)]
    {
        let mode = fs::metadata(out.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o750, "{}", archive.display());
    }
    if links {
        assert_eq!(
            fs::read_link(out.join("link")).unwrap(),
            Path::new("top.txt")
        );
    }
}

#[cfg(unix)]
#[test]
fn zip_from_info_zip_and_ditto() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path(), true);
    sh(&src, "zip -qry ../info.zip .");
    assert_round_trip(&tmp.path().join("info.zip"), None, true);

    // ditto keeps the folder itself and records Unix modes.
    sh(tmp.path(), "ditto -c -k --keepParent src ditto.zip");
    let handle = open_archive(
        tmp.path().join("ditto.zip").display().to_string(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(handle.info().format, "ZIP");
    let out = tmp.path().join("ditto-out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("src/docs/sub/b.txt")).unwrap(),
        "beta"
    );
}

#[cfg(unix)]
#[test]
fn zip_passwords() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path(), false);
    sh(&src, "zip -qr -P secret ../crypto.zip .");

    // AES, as WinZip and 7-Zip write it.
    let aes = tmp.path().join("aes.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&aes).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .with_aes_encryption(zip::AesMode::Aes256, "secret");
    writer.start_file("hidden.txt", options).unwrap();
    writer.write_all(b"aes content").unwrap();
    writer.finish().unwrap();

    for archive in ["crypto.zip", "aes.zip"] {
        let path = tmp.path().join(archive).display().to_string();
        let handle = open_archive(path.clone(), None, None).unwrap();
        assert!(handle.info().has_encrypted_entries, "{archive}");
        let out = tmp.path().join(format!("{archive}-none"));
        assert!(
            matches!(
                extract(&handle, request(&out)),
                Err(CoreError::PasswordRequired)
            ),
            "{archive}"
        );
        assert!(!out.exists());
        let wrong = open_archive(path.clone(), Some("wrong".into()), None).unwrap();
        let out = tmp.path().join(format!("{archive}-wrong"));
        assert!(
            matches!(extract(&wrong, request(&out)), Err(CoreError::BadPassword)),
            "{archive}"
        );
    }
    assert_round_trip(&tmp.path().join("crypto.zip"), Some("secret"), false);
    let handle = open_archive(aes.display().to_string(), Some("secret".into()), None).unwrap();
    let out = tmp.path().join("aes-out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("hidden.txt")).unwrap(),
        "aes content"
    );
}

/// Splits `path` into `{path}.001`, `{path}.002`, … of `size` bytes.
fn split(path: &Path, size: usize) {
    let data = fs::read(path).unwrap();
    for (i, piece) in data.chunks(size).enumerate() {
        fs::write(format!("{}.{:03}", path.display(), i + 1), piece).unwrap();
    }
    fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[test]
fn split_zip_and_7z_open_from_any_piece() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path(), false);
    sh(&src, "zip -qr ../pieces.zip .");
    split(&tmp.path().join("pieces.zip"), 700);
    sevenz_rust2::compress_to_path(&src, tmp.path().join("pieces.7z")).unwrap();
    split(&tmp.path().join("pieces.7z"), 300);
    for opened in ["pieces.zip.002", "pieces.7z.001"] {
        let handle =
            open_archive(tmp.path().join(opened).display().to_string(), None, None).unwrap();
        assert!(handle.info().volumes.len() > 1, "{opened}");
        assert_eq!(handle.info().name, "pieces");
        let out = tmp.path().join(format!("{opened}-out"));
        extract(&handle, request(&out)).unwrap();
        assert_eq!(
            fs::read_to_string(out.join("docs/sub/b.txt")).unwrap(),
            "beta"
        );
    }
}

#[test]
fn sevenz_plain_and_encrypted() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path(), false);
    let plain = tmp.path().join("plain.7z");
    sevenz_rust2::compress_to_path(&src, &plain).unwrap();
    let handle = open_archive(plain.display().to_string(), None, None).unwrap();
    assert_eq!(handle.info().format, "7z");
    let out = tmp.path().join("plain-out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("docs/a.txt")).unwrap(),
        "alpha\n".repeat(1000)
    );
    assert_eq!(fs::read_to_string(out.join("top.txt")).unwrap(), "top");
    assert!(out.join("empty").is_dir());

    let locked = tmp.path().join("locked.7z");
    sevenz_rust2::compress_to_path_encrypted(&src, &locked, "secret".into()).unwrap();
    let path = locked.display().to_string();
    // Encrypted names need the password to open; encrypted contents to extract.
    match open_archive(path.clone(), None, None) {
        Err(CoreError::PasswordRequired) => {}
        Ok(handle) => assert!(matches!(
            extract(&handle, request(&tmp.path().join("none"))),
            Err(CoreError::PasswordRequired | CoreError::BadPassword)
        )),
        Err(other) => panic!("{other}"),
    }
    let wrong = open_archive(path.clone(), Some("wrong".into()), None)
        .and_then(|h| extract(&h, request(&tmp.path().join("wrong"))));
    assert!(matches!(wrong, Err(CoreError::BadPassword)), "{wrong:?}");
    let handle = open_archive(path, Some("secret".into()), None).unwrap();
    let out = tmp.path().join("locked-out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("docs/sub/b.txt")).unwrap(),
        "beta"
    );
}

#[cfg(unix)]
#[test]
fn tar_in_every_compression() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path(), true);
    let mut made = vec![
        ("t.tar", "bsdtar -cf ../t.tar ."),
        ("t.tar.gz", "bsdtar -czf ../t.tar.gz ."),
        ("t.tbz2", "bsdtar -cjf ../t.tbz2 ."),
        ("t.tar.Z", "bsdtar -cf - . | compress -c > ../t.tar.Z"),
    ];
    for (tool, archive, script) in [
        ("xz", "t.txz", "bsdtar -cf - . | xz -T0 -c > ../t.txz"),
        (
            "zstd",
            "t.tar.zst",
            "bsdtar -cf - . | zstd -q -c > ../t.tar.zst",
        ),
        (
            "lz4",
            "t.tar.lz4",
            "bsdtar -cf - . | lz4 -q -c > ../t.tar.lz4",
        ),
    ] {
        if have(tool) {
            made.push((archive, script));
        }
    }
    for (archive, script) in made {
        sh(&src, script);
        let path = tmp.path().join(archive);
        let handle = open_archive(path.display().to_string(), None, None).unwrap();
        assert!(handle.info().format.starts_with("tar"), "{archive}");
        assert_eq!(handle.info().name, "t", "{archive}");
        assert_round_trip(&path, None, true);
    }
}

#[cfg(unix)]
#[test]
fn single_compressed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let text: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
    fs::write(tmp.path().join("notes.txt"), &text).unwrap();
    let mut made = vec![
        ("notes.txt.gz", "gzip -c notes.txt > notes.txt.gz", true),
        ("notes.txt.bz2", "bzip2 -c notes.txt > notes.txt.bz2", false),
        ("notes.txt.Z", "compress -c notes.txt > notes.txt.Z", false),
        // gzip records the original name, which wins over the file's.
        ("renamed.gz", "gzip -c notes.txt > renamed.gz", true),
    ];
    for (tool, archive, script, exact) in [
        ("xz", "notes.txt.xz", "xz -c notes.txt > notes.txt.xz", true),
        (
            "zstd",
            "notes.txt.zst",
            "zstd -q -c notes.txt > notes.txt.zst",
            true,
        ),
        (
            "lz4",
            "notes.txt.lz4",
            "lz4 -q -c notes.txt > notes.txt.lz4",
            false,
        ),
        (
            "brotli",
            "notes.txt.br",
            "brotli -c notes.txt > notes.txt.br",
            false,
        ),
    ] {
        if have(tool) {
            made.push((archive, script, exact));
        }
    }
    for (archive, script, sized) in made {
        sh(tmp.path(), script);
        let handle =
            open_archive(tmp.path().join(archive).display().to_string(), None, None).unwrap();
        let entries = handle.entries();
        assert_eq!(entries.len(), 1, "{archive}");
        assert_eq!(entries[0].name, "notes.txt", "{archive}");
        if sized {
            assert_eq!(handle.info().total_size, text.len() as u64, "{archive}");
        }
        let out = tmp.path().join(format!("{archive}-out"));
        extract(&handle, request(&out)).unwrap();
        assert_eq!(
            fs::read_to_string(out.join("notes.txt")).unwrap(),
            text,
            "{archive}"
        );
    }
}

#[cfg(unix)]
#[test]
fn formats_are_recognized_by_content() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path(), false);
    sh(
        &src,
        "zip -qr ../download.bin . && bsdtar -czf ../backup.dat .",
    );
    for (name, format) in [("download.bin", "ZIP"), ("backup.dat", "tar (gzip)")] {
        let handle = open_archive(tmp.path().join(name).display().to_string(), None, None).unwrap();
        assert_eq!(handle.info().format, format);
    }
    fs::write(tmp.path().join("plain.txt"), "not an archive").unwrap();
    assert!(matches!(
        open_archive(
            tmp.path().join("plain.txt").display().to_string(),
            None,
            None
        ),
        Err(CoreError::Unsupported { .. })
    ));
}

/// tar stores names as given; one climbing out of the destination is listed
/// as unsafe and never written.
#[test]
fn tar_traversal_names_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("evil.tar");
    let mut builder = tar::Builder::new(fs::File::create(&path).unwrap());
    for name in ["ok.txt", "../escape.txt", "/abs.txt"] {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b"hi"[..]).unwrap();
    }
    builder.finish().unwrap();
    drop(builder);
    let handle = open_archive(path.display().to_string(), None, None).unwrap();
    assert_eq!(handle.info().unsafe_names, ["../escape.txt", "/abs.txt"]);
    let out = tmp.path().join("deep/out");
    let summary = extract(&handle, request(&out)).unwrap();
    assert_eq!(tree(&out), ["ok.txt"]);
    assert_eq!(summary.skipped_unsafe.len(), 2);
    assert!(!tmp.path().join("deep/escape.txt").exists());
}

#[cfg(unix)]
#[test]
fn cancelling_a_parallel_zip_leaves_nothing_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir(&src).unwrap();
    for i in 0..64 {
        fs::write(
            src.join(format!("f{i}.txt")),
            vec![b'a' + (i % 26) as u8; 100_000],
        )
        .unwrap();
    }
    sh(&src, "zip -qr ../many.zip .");
    let handle = open_archive(
        tmp.path().join("many.zip").display().to_string(),
        None,
        None,
    )
    .unwrap();
    let token = CancelToken::new();
    let recorder = Arc::new(Recorder {
        cancel_on_first: Some(token.clone()),
        ..Default::default()
    });
    let out = tmp.path().join("out");
    let result = handle.extract(request(&out), recorder, token);
    assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
    assert!(!out.exists());
}

/// Finder and ditto keep extended attributes in `__MACOSX/._name` files;
/// those are hidden and their attributes restored, as Archive Utility does.
#[cfg(unix)]
#[test]
fn zip_restores_apple_double_attributes() {
    let tmp = tempfile::tempdir().unwrap();
    source_tree(tmp.path(), false);
    sh(
        tmp.path(),
        "xattr -w com.example.note hello src/top.txt && xattr -w com.example.dir yes src/docs \
         && ditto -c -k --sequesterRsrc --keepParent src mac.zip",
    );
    let handle =
        open_archive(tmp.path().join("mac.zip").display().to_string(), None, None).unwrap();
    assert!(
        handle
            .entries()
            .iter()
            .all(|e| !e.path.starts_with("__MACOSX"))
    );
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    assert!(!out.join("__MACOSX").exists());
    for (path, name, value) in [
        ("src/top.txt", "com.example.note", "hello"),
        ("src/docs", "com.example.dir", "yes"),
    ] {
        let read = Command::new("xattr")
            .args(["-p", name])
            .arg(out.join(path))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&read.stdout).trim(),
            value,
            "{path}"
        );
    }
}
