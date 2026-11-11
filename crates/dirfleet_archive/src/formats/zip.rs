//! ZIP and the formats built on it (CBZ, JAR, APK, EPUB, IPA), through the
//! zip crate. Members are independent, so wanted files decode in parallel.
//! AppleDouble companions in `__MACOSX` are hidden, and what they hold is
//! restored onto the files they describe.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::io::Read;
use std::time::SystemTime;

use rayon::prelude::*;
use zip::ZipArchive;
use zip::result::ZipError;

use super::{Backend, Job, Link, Listing, Sink, appledouble, copy, read_error, read_target};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names::{self, Encoding};
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) struct Zip {
    archive: ZipArchive<Source>,
    /// For each member, its AppleDouble companion.
    companions: Vec<Option<usize>>,
}

pub(crate) fn open(volumes: VolumeSet, encoding: Encoding) -> Result<(Zip, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let archive = ZipArchive::new(source).map_err(zip_error)?;
    let mut members = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index_data(index).map_err(zip_error)?;
        let name = names::decode(entry.name_raw(), encoding);
        let unix = entry.system() == zip::System::Unix;
        members.push(Member {
            rel_path: names::safe_relative_path(&name, true),
            display: String::from_utf8_lossy(&name).into_owned(),
            entry_id: None,
            kind: if entry.is_dir() {
                Kind::Dir
            } else if entry.is_symlink() {
                Kind::Link
            } else {
                Kind::File
            },
            size: entry.size(),
            packed_size: entry.compressed_size(),
            mtime: entry.last_modified().and_then(dos_time),
            mode: entry.unix_mode().filter(|_| unix).map(|m| m & 0o777),
            encrypted: entry.encrypted(),
            hidden: false,
        });
    }
    let companions = pair_companions(&mut members);
    let comment = Some(
        String::from_utf8_lossy(archive.comment())
            .trim()
            .to_string(),
    )
    .filter(|c| !c.is_empty());
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: "ZIP".to_string(),
        solid: false,
        comment,
        sizes_exact: true,
        packed_size: None,
    };
    Ok((
        Zip {
            archive,
            companions,
        },
        listing,
    ))
}

/// Hides everything in `__MACOSX` and pairs each companion with its file.
fn pair_companions(members: &mut [Member]) -> Vec<Option<usize>> {
    let mut companions = vec![None; members.len()];
    let by_name: HashMap<&str, usize> = members
        .iter()
        .enumerate()
        .filter(|(_, m)| !appledouble::is_companion_area(&m.display))
        .map(|(i, m)| (m.display.trim_end_matches('/'), i))
        .collect();
    let mut hidden = Vec::new();
    for (index, member) in members.iter().enumerate() {
        if appledouble::is_companion_area(&member.display) {
            hidden.push(index);
            if let Some(target) = appledouble::target_of(member.display.trim_end_matches('/'))
                && let Some(&file) = by_name.get(target.as_str())
            {
                companions[file] = Some(index);
            }
        }
    }
    for index in hidden {
        members[index].hidden = true;
    }
    companions
}

impl Backend for Zip {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let mut files = Vec::new();
        for (index, member) in job.members.iter().enumerate() {
            if !job.wanted[index] {
                continue;
            }
            match member.kind {
                Kind::Dir => drop(sink.open(index)?),
                _ => files.push(index),
            }
        }
        // Large files first, so one doesn't start last and finish alone.
        files.sort_by_key(|&index| Reverse(job.members[index].size));
        files.par_iter().try_for_each_init(
            || self.archive.clone(),
            |archive, &index| extract_one(archive, index, job, sink),
        )?;
        // Resource forks and extended attributes have nowhere to go on Windows.
        if cfg!(windows) {
            return Ok(());
        }
        let mut archive = self.archive.clone();
        for (index, companion) in self.companions.iter().enumerate() {
            if let Some(companion) = *companion
                && job.wanted[index]
            {
                job.check_cancelled()?;
                let attributes = appledouble::parse(&read_member(&mut archive, companion, job)?);
                if !attributes.is_empty() {
                    sink.xattrs(index, attributes);
                }
            }
        }
        Ok(())
    }
}

fn read_member(archive: &mut ZipArchive<Source>, index: usize, job: &Job<'_>) -> Result<Vec<u8>> {
    let encrypted = job.members[index].encrypted;
    let file = match job.password {
        Some(password) if encrypted => archive.by_index_decrypt(index, password),
        _ => archive.by_index(index),
    };
    let mut data = Vec::new();
    file.map_err(zip_error)?
        .read_to_end(&mut data)
        .map_err(|e| read_error(e, encrypted))?;
    Ok(data)
}

fn extract_one(
    archive: &mut ZipArchive<Source>,
    index: usize,
    job: &Job<'_>,
    sink: &dyn Sink,
) -> Result<()> {
    job.check_cancelled()?;
    let member = &job.members[index];
    let file = match job.password {
        Some(password) if member.encrypted => archive.by_index_decrypt(index, password),
        _ => archive.by_index(index),
    };
    let mut file = file.map_err(|e| match e {
        // ZipCrypto has only a one-byte password check; a wrong password
        // usually fails it, but may pass and fail the checksum instead.
        ZipError::Io(e) => read_error(e, member.encrypted),
        other => zip_error(other),
    })?;
    if member.kind == Kind::Link {
        let target = read_target(&mut file, member.encrypted)?;
        return sink.link(index, Link::Symlink(&target));
    }
    let mut writer = sink.open(index)?;
    copy(&mut file, &mut writer, job, member.encrypted)
}

fn zip_error(error: ZipError) -> CoreError {
    match error {
        ZipError::InvalidPassword => CoreError::BadPassword,
        ZipError::UnsupportedArchive(message) if message.contains("assword") => {
            CoreError::PasswordRequired
        }
        ZipError::UnsupportedArchive(message) => CoreError::Unsupported {
            message: message.to_string(),
        },
        ZipError::CompressionMethodNotSupported(method) => CoreError::Unsupported {
            message: format!("compression method {method}"),
        },
        ZipError::Io(error) => read_error(error, false),
        other => CoreError::Corrupt {
            message: other.to_string(),
        },
    }
}

/// An MS-DOS timestamp, which is local time, as an instant.
fn dos_time(datetime: zip::DateTime) -> Option<SystemTime> {
    let (date, time): (u16, u16) = datetime.into();
    super::local_time(
        i32::from(date >> 9) + 1980,
        ((date >> 5) & 0xf) as u8,
        (date & 0x1f) as u8,
        (time >> 11) as u8,
        ((time >> 5) & 0x3f) as u8,
        ((time & 0x1f) * 2) as u8,
    )
}
