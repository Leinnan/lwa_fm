//! Getting `FFmpeg` for video previews when it isn't installed.
//!
//! A download into
//! the app's data folder (not next to the executable, which may be read-only),
//! verified against the published SHA-256 when the provider offers one.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where downloaded `FFmpeg` tools live.
pub fn managed_dir() -> Option<PathBuf> {
    Some(super::dirs().data.join("ffmpeg"))
}

/// A downloaded tool (`ffmpeg` or `ffprobe`), if present.
pub fn managed_tool(name: &str) -> Option<PathBuf> {
    let mut path = managed_dir()?.join(name);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path.is_file().then_some(path)
}

/// Downloads and unpacks `FFmpeg` into [`managed_dir`]. Tests never download.
///
/// Uses the system `curl` and `tar` (both ship with Windows 10+, macOS and
/// Linux) instead of an in-process HTTP/TLS/archive stack, which would add
/// megabytes to the binary for a one-off download.
#[cfg(not(any(test, feature = "test-support")))]
pub fn install() -> anyhow::Result<()> {
    let dir = managed_dir().ok_or_else(|| anyhow::anyhow!("No data folder for FFmpeg"))?;
    fs::create_dir_all(&dir)?;
    let url = ffmpeg_sidecar::download::ffmpeg_download_url()?;
    let archive = download(url, &dir)?;
    if let Some(expected) = published_sha256(url, &dir) {
        let actual = sha256_hex(&archive)?;
        if !actual.eq_ignore_ascii_case(&expected) {
            let _ = fs::remove_file(&archive);
            anyhow::bail!("FFmpeg download failed its checksum ({actual} != {expected})");
        }
    } else {
        log::warn!("No published checksum for {url}; installing unverified");
    }
    let unpacked = unpack(&archive, &dir);
    let _ = fs::remove_file(&archive);
    unpacked?;
    if managed_tool("ffmpeg").is_none() {
        anyhow::bail!("FFmpeg archive did not contain ffmpeg");
    }
    Ok(())
}

/// Fetches `url` into `dir` with the system `curl`, keeping the URL's file name.
#[cfg(not(any(test, feature = "test-support")))]
fn download(url: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = url.rsplit('/').next().filter(|name| !name.is_empty());
    let path = dir.join(name.unwrap_or("ffmpeg_download"));
    run(system_tool("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "2",
        ])
        .arg("--output")
        .arg(&path)
        .arg(url))?;
    Ok(path)
}

/// Extracts `archive` with the system `tar` (bsdtar reads the Windows and
/// macOS zips, GNU tar the Linux .tar.xz) and moves `ffmpeg` and `ffprobe`
/// from wherever the archive keeps them into `dir`.
#[cfg(not(any(test, feature = "test-support")))]
fn unpack(archive: &Path, dir: &Path) -> anyhow::Result<()> {
    let temp = dir.join("unpack_temp");
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    let result = run(system_tool("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&temp))
    .and_then(|()| move_tools(&temp, dir));
    let _ = fs::remove_dir_all(&temp);
    result
}

#[cfg(not(any(test, feature = "test-support")))]
fn move_tools(from: &Path, to: &Path) -> anyhow::Result<()> {
    let wanted = |name: &str| {
        let stem = name.strip_suffix(".exe").unwrap_or(name);
        stem == "ffmpeg" || stem == "ffprobe"
    };
    for entry in walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(Result::ok)
    {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file() && wanted(&name) {
            fs::rename(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// A tool that ships with the OS. On Windows it is taken from System32, so a
/// different `tar` on `PATH` (Git's can't read zips) isn't picked up.
#[cfg(not(any(test, feature = "test-support")))]
fn system_tool(name: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// Run without opening a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let executable = std::env::var_os("SystemRoot").map_or_else(
            || PathBuf::from(format!("{name}.exe")),
            |root| {
                PathBuf::from(root)
                    .join("System32")
                    .join(format!("{name}.exe"))
            },
        );
        let mut command = std::process::Command::new(executable);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    std::process::Command::new(name)
}

#[cfg(not(any(test, feature = "test-support")))]
fn run(command: &mut std::process::Command) -> anyhow::Result<()> {
    let output = command.stdin(std::process::Stdio::null()).output()?;
    if !output.status.success() {
        anyhow::bail!(
            "{} failed: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// The SHA-256 the provider publishes next to the archive. Only gyan.dev
/// (the Windows build) publishes one; other providers return `None`.
#[cfg(not(any(test, feature = "test-support")))]
fn published_sha256(url: &str, dir: &Path) -> Option<String> {
    if !url.contains("gyan.dev") {
        return None;
    }
    let checksum_file = download(&format!("{url}.sha256"), dir).ok()?;
    let text = fs::read_to_string(&checksum_file).ok();
    let _ = fs::remove_file(&checksum_file);
    parse_sha256(&text?)
}

/// The first 64-hex-digit word of a checksum file.
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn parse_sha256(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|word| word.len() == 64 && word.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn sha256_hex(path: &Path) -> std::io::Result<String> {
    use sha2::Digest as _;
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::{parse_sha256, sha256_hex};

    #[test]
    fn checksums_are_parsed_and_computed() {
        let digest = "a".repeat(64);
        assert_eq!(parse_sha256(&format!("{digest}  file.zip\n")), Some(digest));
        assert_eq!(parse_sha256("not a checksum"), None);
        let path = std::env::temp_dir().join(format!("lwa_fm_sha_{}", std::process::id()));
        std::fs::write(&path, b"abc").expect("write");
        assert_eq!(
            sha256_hex(&path).expect("hash"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(path);
    }
}
