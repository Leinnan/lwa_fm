//! Making an archive of files and folders, as a file operation.
//!
//! The archive is written under another name beside where it goes and
//! renamed at the end, so one that fails or is stopped leaves nothing. Every
//! byte goes through a reader that reports progress and holds or stops the
//! operation when told to.
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dirfleet_core::fileops::{ArchiveFormat, FileOp, FileOpEvent, Origin, Outcome, Undo};

use crate::fileops::OpControl;

/// How often progress is told.
const REPORT_INTERVAL: Duration = Duration::from_millis(60);
/// Files are read in pieces of this size.
const CHUNK: usize = 1024 * 1024;

/// One thing that goes into the archive.
struct Item {
    path: PathBuf,
    /// Its path inside the archive, separated by `/`.
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
}

/// Everything under `srcs`, folders before what they hold, and how many
/// links were left out: a link may lead anywhere.
fn gather(srcs: &[PathBuf]) -> (Vec<Item>, usize) {
    let mut items = Vec::new();
    let mut links = 0;
    for src in srcs {
        let Some(base) = src.parent() else {
            continue;
        };
        for entry in walkdir::WalkDir::new(src).sort_by_file_name() {
            let Ok(entry) = entry else {
                continue;
            };
            if entry.path_is_symlink() {
                links += 1;
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Ok(rel) = entry.path().strip_prefix(base) else {
                continue;
            };
            let name = rel
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            items.push(Item {
                path: entry.path().to_path_buf(),
                name,
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().ok(),
            });
        }
    }
    (items, links)
}

/// Counts what is read, tells it now and then, and stops the read when the
/// operation is stopped.
struct Meter<'a> {
    id: u64,
    control: &'a OpControl,
    report: &'a dyn Fn(FileOpEvent),
    done: u64,
    total: u64,
    current: String,
    last: Instant,
}

impl Meter<'_> {
    fn tell(&mut self, force: bool) {
        if !force && self.last.elapsed() < REPORT_INTERVAL {
            return;
        }
        self.last = Instant::now();
        (self.report)(FileOpEvent::Progress {
            id: self.id,
            fraction: if self.total == 0 {
                0.0
            } else {
                (self.done as f64 / self.total as f64).min(1.0) as f32
            },
            current: self.current.clone(),
            bytes_done: self.done,
            bytes_total: self.total,
            bytes_per_second: 0,
        });
    }
}

/// Why a read ended early. Not `Interrupted`: readers try those again.
fn stopped() -> io::Error {
    io::Error::other("stopped")
}

/// A file being read into the archive.
struct Metered<'m, 'a> {
    file: File,
    meter: &'m mut Meter<'a>,
}

impl Read for Metered<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // Held here while paused.
        if self.meter.control.wait_if_paused() {
            return Err(stopped());
        }
        let wanted = buffer.len().min(CHUNK);
        let read = self.file.read(&mut buffer[..wanted])?;
        self.meter.done += read as u64;
        self.meter.tell(false);
        Ok(read)
    }
}

impl<'a> Meter<'a> {
    /// Opens `item` for reading into the archive.
    fn open<'m>(&'m mut self, item: &Item) -> io::Result<Metered<'m, 'a>> {
        self.current.clone_from(&item.name);
        self.tell(false);
        Ok(Metered {
            file: File::open(&item.path)?,
            meter: self,
        })
    }
}

/// Days since 1970-01-01 as a date (Howard Hinnant's `civil_from_days`).
const fn civil(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u8;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u8;
    let year = year_of_era + era * 400 + (month <= 2) as i64;
    (year, month, day)
}

/// A file's time as a zip keeps it. Zips have no time zone: this is UTC.
fn zip_time(modified: Option<SystemTime>) -> Option<zip::DateTime> {
    let seconds = modified?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (year, month, day) = civil(i64::try_from(seconds / 86_400).ok()?);
    let of_day = seconds % 86_400;
    zip::DateTime::from_date_and_time(
        u16::try_from(year).ok()?,
        month,
        day,
        (of_day / 3600) as u8,
        (of_day % 3600 / 60) as u8,
        (of_day % 60) as u8,
    )
    .ok()
}

fn write_zip(sink: BufWriter<File>, items: &[Item], meter: &mut Meter<'_>) -> io::Result<()> {
    let mut zip = zip::ZipWriter::new(sink);
    for item in items {
        let mut options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(item.size >= u64::from(u32::MAX));
        if let Some(time) = zip_time(item.modified) {
            options = options.last_modified_time(time);
        }
        if item.is_dir {
            zip.add_directory(item.name.as_str(), options)
                .map_err(io::Error::other)?;
        } else {
            zip.start_file(item.name.as_str(), options)
                .map_err(io::Error::other)?;
            io::copy(&mut meter.open(item)?, &mut zip)?;
        }
    }
    zip.finish().map_err(io::Error::other)?.flush()
}

fn write_tar<W: Write>(sink: W, items: &[Item], meter: &mut Meter<'_>) -> io::Result<W> {
    let mut tar = tar::Builder::new(sink);
    for item in items {
        if item.is_dir {
            tar.append_dir(&item.name, &item.path)?;
        } else {
            let mut header = tar::Header::new_gnu();
            header.set_metadata(&fs::metadata(&item.path)?);
            // What is read is what is written, even if the file grew since.
            header.set_size(item.size);
            let reader = meter.open(item)?.take(item.size);
            tar.append_data(&mut header, &item.name, reader)?;
        }
    }
    tar.into_inner()
}

fn write_seven_zip(file: File, items: &[Item], meter: &mut Meter<'_>) -> io::Result<()> {
    let mut archive = sevenz_rust2::ArchiveWriter::new(file).map_err(io::Error::other)?;
    for item in items {
        let entry = sevenz_rust2::ArchiveEntry::from_path(&item.path, item.name.clone());
        if item.is_dir {
            archive
                .push_archive_entry::<File>(entry, None)
                .map_err(io::Error::other)?;
        } else {
            archive
                .push_archive_entry(entry, Some(meter.open(item)?))
                .map_err(io::Error::other)?;
        }
    }
    archive.finish()?;
    Ok(())
}

/// Writes `items` to a new file at `path` as a `format` archive.
fn write(
    path: &Path,
    format: ArchiveFormat,
    items: &[Item],
    meter: &mut Meter<'_>,
) -> io::Result<()> {
    let file = File::create_new(path)?;
    match format {
        ArchiveFormat::Zip => write_zip(BufWriter::new(file), items, meter),
        ArchiveFormat::SevenZip => write_seven_zip(file, items, meter),
        ArchiveFormat::TarGz => {
            let packed =
                flate2::write::GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
            write_tar(packed, items, meter)?.finish()?.flush()
        }
        ArchiveFormat::TarZst => {
            let packed = zstd::stream::write::Encoder::new(BufWriter::new(file), 0)?;
            write_tar(packed, items, meter)?.finish()?.flush()
        }
        ArchiveFormat::TarXz => {
            let packed = lzma_rust2::XzWriter::new(
                BufWriter::new(file),
                lzma_rust2::XzOptions::with_preset(6),
            )?;
            write_tar(packed, items, meter)?.finish()?.flush()
        }
    }
}

fn count(items: usize) -> String {
    if items == 1 {
        "1 link".to_owned()
    } else {
        format!("{items} links")
    }
}

/// Runs a [`FileOp::Compress`] to its end on the calling thread.
pub fn run(
    id: u64,
    op: &FileOp,
    origin: Origin,
    control: &OpControl,
    report: &dyn Fn(FileOpEvent),
) -> Outcome {
    let description = op.describe();
    report(FileOpEvent::Started {
        id,
        description: description.clone(),
    });
    let mut outcome = Outcome {
        id,
        origin,
        description,
        errors: Vec::new(),
        cancelled: false,
        undo: None,
        affected_dirs: Vec::new(),
        created: Vec::new(),
        trash_changed: false,
        verified: false,
        needs_password: None,
    };
    let FileOp::Compress { srcs, dst, format } = op else {
        outcome
            .errors
            .push("Only an archive is made here".to_owned());
        return outcome;
    };
    let name = dirfleet_core::fileops::display_name(dst);
    if dst.symlink_metadata().is_ok() {
        outcome.errors.push(format!("{name} already exists"));
        return outcome;
    }
    let (items, links) = gather(srcs);
    let mut meter = Meter {
        id,
        control,
        report,
        done: 0,
        total: items.iter().map(|item| item.size).sum(),
        current: String::new(),
        last: Instant::now()
            .checked_sub(REPORT_INTERVAL)
            .unwrap_or_else(Instant::now),
    };
    // Under another name until it is whole.
    let partial = dst.with_file_name(format!(".{name}.partial"));
    let _ = fs::remove_file(&partial);
    let written =
        write(&partial, *format, &items, &mut meter).and_then(|()| fs::rename(&partial, dst));
    match written {
        Ok(()) => {
            meter.tell(true);
            outcome
                .affected_dirs
                .extend(dst.parent().map(Path::to_path_buf));
            outcome.created.push(dst.clone());
            outcome.undo = Some(Undo::Created(vec![dst.clone()]));
            if links > 0 {
                outcome
                    .errors
                    .push(format!("{} left out of {name}", count(links)));
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            if control.is_cancelled() {
                outcome.cancelled = true;
            } else {
                outcome.errors.push(format!("{name}: {error}"));
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::Vfs;
    use dirfleet_core::location::Location;

    /// A folder with a file beside it, a file inside it and one further down.
    fn tree(dir: &Path) -> Vec<PathBuf> {
        let folder = dir.join("docs");
        fs::create_dir_all(folder.join("deep")).expect("folders");
        fs::write(dir.join("readme.txt"), b"hello").expect("write");
        fs::write(folder.join("a.md"), b"# a").expect("write");
        fs::write(folder.join("deep/notes.md"), vec![b'n'; 300_000]).expect("write");
        vec![dir.join("readme.txt"), folder]
    }

    fn compress(srcs: &[PathBuf], dst: &Path, format: ArchiveFormat) -> Outcome {
        let op = FileOp::Compress {
            srcs: srcs.to_vec(),
            dst: dst.to_path_buf(),
            format,
        };
        run(1, &op, Origin::User, &OpControl::default(), &|_| {})
    }

    #[test]
    fn every_format_makes_an_archive_that_reads_back_the_same() {
        let dir = tempfile::tempdir().expect("temp dir");
        let srcs = tree(dir.path());
        let vfs = Vfs::new(dir.path().join("cache"));
        for format in ArchiveFormat::ALL {
            let dst = dir.path().join(format!("out.{}", format.extension()));
            let outcome = compress(&srcs, &dst, format);
            assert!(outcome.succeeded(), "{format:?}: {outcome:?}");
            assert_eq!(outcome.created, std::slice::from_ref(&dst));
            assert!(
                matches!(&outcome.undo, Some(Undo::Created(made)) if made == std::slice::from_ref(&dst))
            );

            let top = Location::Local(dst.to_string_lossy().into_owned()).into_archive();
            let listing = vfs
                .listing(&top)
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let mut names: Vec<&str> = (0..listing.len()).map(|i| listing.name(i)).collect();
            names.sort_unstable();
            assert_eq!(names, ["docs", "readme.txt"], "{format:?}");
            let notes = vfs
                .local_file(&top.join("docs").join("deep").join("notes.md"))
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(
                fs::read(notes).expect("read"),
                vec![b'n'; 300_000],
                "{format:?}"
            );
        }
        // Nothing half-made is left beside them.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn an_archive_is_never_written_over_another() {
        let dir = tempfile::tempdir().expect("temp dir");
        let srcs = tree(dir.path());
        let dst = dir.path().join("out.zip");
        fs::write(&dst, b"mine").expect("write");
        let outcome = compress(&srcs, &dst, ArchiveFormat::Zip);
        assert_eq!(outcome.errors, ["out.zip already exists"]);
        assert_eq!(fs::read(&dst).expect("read"), b"mine");
    }

    #[test]
    fn a_stopped_archive_is_removed_and_progress_counts_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let srcs = tree(dir.path());
        let dst = dir.path().join("out.tar.gz");
        let control = OpControl::default();
        let op = FileOp::Compress {
            srcs,
            dst: dst.clone(),
            format: ArchiveFormat::TarGz,
        };
        let total = std::cell::Cell::new(0);
        let outcome = run(1, &op, Origin::User, &control, &|event| {
            if let FileOpEvent::Progress { bytes_total, .. } = event {
                total.set(bytes_total);
                control.cancel();
            }
        });
        assert!(outcome.cancelled, "{outcome:?}");
        assert_eq!(total.get(), 5 + 3 + 300_000);
        assert!(!dst.exists());
        assert_eq!(
            fs::read_dir(dir.path())
                .expect("read")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains("out.tar.gz"))
                .count(),
            0,
            "nothing half-made is left"
        );
    }

    #[test]
    fn days_since_1970_are_told_as_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_782), (2024, 2, 29));
        assert_eq!(civil(20_454), (2026, 1, 1));
    }
}
