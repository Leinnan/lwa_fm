//! Unix and Apple formats: cpio, ar, Debian and RPM packages, XAR and
//! installer packages, cabinets and disk images. Fixtures come from the
//! macOS tools that make them (cpio, ar, xar, pkgbuild, productbuild,
//! hdiutil); RPM, which has none on macOS, is assembled by hand.

// Most of these tests run Unix tools; the helpers are unused without them.
#![cfg_attr(not(unix), allow(dead_code, unused_imports, unused_variables))]

use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use dirfleet_archive::{CoreError, open_archive};

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

fn source_tree(root: &Path) -> std::path::PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("docs/sub")).unwrap();
    fs::write(src.join("docs/a.txt"), "alpha\n".repeat(1000)).unwrap();
    fs::write(src.join("docs/sub/b.txt"), "beta").unwrap();
    fs::write(src.join("run.sh"), "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("run.sh", src.join("link")).unwrap();
    src
}

fn open(path: &Path, password: Option<&str>) -> std::sync::Arc<dirfleet_archive::ArchiveHandle> {
    open_archive(
        path.display().to_string(),
        password.map(str::to_string),
        None,
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Extracts `archive` and checks the files of `source_tree` under `under`.
fn assert_extracts(archive: &Path, password: Option<&str>, under: &str, modes: bool) -> String {
    let handle = open(archive, password);
    let out = archive.with_extension("out");
    let _ = fs::remove_dir_all(&out);
    extract(&handle, request(&out)).unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
    let root = out.join(under);
    assert_eq!(
        fs::read_to_string(root.join("docs/a.txt")).unwrap(),
        "alpha\n".repeat(1000),
        "{}",
        archive.display()
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/sub/b.txt")).unwrap(),
        "beta"
    );
    #[cfg(unix)]
    if modes {
        let mode = fs::metadata(root.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o750, "{}", archive.display());
        assert_eq!(
            fs::read_link(root.join("link")).unwrap(),
            Path::new("run.sh")
        );
    }
    handle.info().format
}

#[cfg(unix)]
#[test]
fn cpio_in_each_format() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path());
    for (name, script) in [
        (
            "odc.cpio",
            "find . | cpio -o -H odc > ../odc.cpio 2>/dev/null",
        ),
        (
            "newc.cpio",
            "find . | cpio -o -H newc > ../newc.cpio 2>/dev/null",
        ),
        (
            "bin.cpio",
            "find . | cpio -o -H bin > ../bin.cpio 2>/dev/null",
        ),
        (
            "newc.cpio.gz",
            "find . | cpio -o -H newc 2>/dev/null | gzip > ../newc.cpio.gz",
        ),
    ] {
        sh(&src, script);
        let format = assert_extracts(&tmp.path().join(name), None, "", true);
        assert!(format.starts_with("cpio"), "{name}: {format}");
    }
}

#[cfg(unix)]
#[test]
fn ar_and_debian_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path());
    // A static library: members are plain files.
    sh(
        tmp.path(),
        "printf one > first.o && printf two > second.o && ar rcS lib.a first.o second.o",
    );
    let handle = open(&tmp.path().join("lib.a"), None);
    assert_eq!(handle.info().format, "ar");
    let out = tmp.path().join("lib-out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(tree(&out), ["first.o", "second.o"]);
    assert_eq!(fs::read_to_string(out.join("second.o")).unwrap(), "two");

    // A Debian package: data at the root, control files under DEBIAN.
    let pkg = tmp.path().join("pkg");
    fs::create_dir(&pkg).unwrap();
    fs::write(pkg.join("control"), "Package: demo\n").unwrap();
    fs::write(pkg.join("debian-binary"), "2.0\n").unwrap();
    sh(&pkg, "bsdtar -czf control.tar.gz control");
    sh(&src, "bsdtar -cJf ../pkg/data.tar.xz .");
    sh(
        &pkg,
        "ar rcS ../demo.deb debian-binary control.tar.gz data.tar.xz",
    );
    let format = assert_extracts(&tmp.path().join("demo.deb"), None, "", true);
    assert_eq!(format, "Debian package 2.0");
    let out = tmp.path().join("demo.out");
    assert_eq!(
        fs::read_to_string(out.join("DEBIAN/control")).unwrap(),
        "Package: demo\n"
    );
}

/// The smallest RPM: a lead, an empty signature header, a main header with
/// a name and summary, and a gzip cpio payload.
fn write_rpm(path: &Path, payload: &[u8]) {
    fn header(tags: &[(u32, &str)]) -> Vec<u8> {
        let mut index = Vec::new();
        let mut store = Vec::new();
        for (tag, value) in tags {
            index.extend(tag.to_be_bytes());
            index.extend(6u32.to_be_bytes());
            index.extend((store.len() as u32).to_be_bytes());
            index.extend(1u32.to_be_bytes());
            store.extend(value.as_bytes());
            store.push(0);
        }
        let mut out = vec![0x8e, 0xad, 0xe8, 0x01, 0, 0, 0, 0];
        out.extend((tags.len() as u32).to_be_bytes());
        out.extend((store.len() as u32).to_be_bytes());
        out.extend(index);
        out.extend(store);
        out
    }
    let mut rpm = vec![0xed, 0xab, 0xee, 0xdb];
    rpm.resize(96, 0);
    let signature = header(&[]);
    rpm.extend(&signature);
    rpm.resize(rpm.len().div_ceil(8) * 8, 0);
    rpm.extend(header(&[
        (1000, "demo"),
        (1001, "1.0"),
        (1002, "1"),
        (1004, "A demo"),
    ]));
    rpm.extend(payload);
    fs::File::create(path).unwrap().write_all(&rpm).unwrap();
}

#[cfg(unix)]
#[test]
fn rpm_lists_its_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let src = source_tree(tmp.path());
    sh(
        &src,
        "find . | cpio -o -H newc 2>/dev/null | gzip > ../payload.gz",
    );
    let rpm = tmp.path().join("demo-1.0-1.noarch.rpm");
    write_rpm(&rpm, &fs::read(tmp.path().join("payload.gz")).unwrap());
    assert_extracts(&rpm, None, "", true);
    assert_eq!(
        open(&rpm, None).info().comment.as_deref(),
        Some("demo-1.0-1: A demo")
    );
}

#[cfg(unix)]
#[test]
fn xar_and_installer_packages() {
    let tmp = tempfile::tempdir().unwrap();
    source_tree(tmp.path());
    sh(
        tmp.path(),
        "cd src && xar -cf ../plain.xar . && xar -cf ../bz.xar --compression bzip2 .",
    );
    for name in ["plain.xar", "bz.xar"] {
        assert_eq!(
            assert_extracts(&tmp.path().join(name), None, "", true),
            "XAR"
        );
    }
    // A component package, and a product archive holding it.
    sh(
        tmp.path(),
        "pkgbuild --quiet --root src --identifier com.example.demo --version 1 \
         --install-location /tmp/demo component.pkg \
         && productbuild --quiet --package component.pkg product.pkg",
    );
    let format = assert_extracts(&tmp.path().join("component.pkg"), None, "Payload", true);
    assert_eq!(format, "Installer package");
    let handle = open(&tmp.path().join("product.pkg"), None);
    let paths: Vec<_> = handle.entries().into_iter().map(|e| e.path).collect();
    assert!(paths.iter().any(|p| p == "Distribution"), "{paths:?}");
    assert!(
        paths
            .iter()
            .any(|p| p == "component.pkg/Payload/docs/sub/b.txt"),
        "{paths:?}"
    );
}

#[test]
fn cabinets() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("demo.cab");
    let mut builder = cab::CabinetBuilder::new();
    let folder = builder.add_folder(cab::CompressionType::MsZip);
    folder.add_file("docs\\a.txt");
    folder.add_file("b.txt");
    let mut writer = builder.build(fs::File::create(&path).unwrap()).unwrap();
    let contents = ["alpha\n".repeat(1000), "beta".to_string()];
    let mut i = 0;
    while let Some(mut file) = writer.next_file().unwrap() {
        file.write_all(contents[i].as_bytes()).unwrap();
        i += 1;
    }
    writer.finish().unwrap();
    let handle = open(&path, None);
    assert_eq!(handle.info().format, "Cabinet");
    let out = tmp.path().join("out");
    extract(&handle, request(&out)).unwrap();
    assert_eq!(tree(&out), ["b.txt", "docs/", "docs/a.txt"]);
    assert_eq!(
        fs::read_to_string(out.join("docs/a.txt")).unwrap(),
        contents[0]
    );
}

/// Whether `image` is attached, according to hdiutil.
fn attached(image: &Path) -> bool {
    let info = Command::new("hdiutil").arg("info").output().unwrap();
    let image = fs::canonicalize(image).unwrap();
    String::from_utf8_lossy(&info.stdout).contains(&image.display().to_string())
}

#[cfg(unix)]
#[test]
fn disk_images_are_mounted_read_and_detached() {
    let tmp = tempfile::tempdir().unwrap();
    source_tree(tmp.path());
    sh(
        tmp.path(),
        "hdiutil create -quiet -srcfolder src -volname Demo -format UDZO demo.dmg \
         && hdiutil makehybrid -quiet -iso -joliet -o disc.iso src \
         && printf secret | hdiutil create -quiet -srcfolder src -volname Locked -format UDZO \
            -encryption AES-128 -stdinpass locked.dmg",
    );
    let dmg = tmp.path().join("demo.dmg");
    assert_eq!(assert_extracts(&dmg, None, "", true), "Disk image");
    assert_eq!(
        assert_extracts(&tmp.path().join("disc.iso"), None, "", false),
        "Disc image"
    );
    assert!(
        !attached(&dmg),
        "the image is detached once the handle is gone"
    );

    let locked = tmp.path().join("locked.dmg");
    let path = locked.display().to_string();
    assert!(matches!(
        open_archive(path.clone(), None, None),
        Err(CoreError::PasswordRequired)
    ));
    assert!(matches!(
        open_archive(path, Some("wrong".into()), None),
        Err(CoreError::BadPassword)
    ));
    assert_extracts(&locked, Some("secret"), "", true);
    assert!(!attached(&locked));
}
