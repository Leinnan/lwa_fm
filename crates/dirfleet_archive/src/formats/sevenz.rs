//! 7z (and CB7) through sevenz-rust2. Files are compressed in blocks, one
//! per file in a non-solid archive and many per block in a solid one; blocks
//! decode independently, so the blocks that hold wanted files run in
//! parallel. Inside a block everything up to the last wanted file decodes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::time::SystemTime;

use rayon::prelude::*;
use sevenz_rust2::{Archive, ArchiveEntry, BlockDecoder, Password};

use super::{Backend, Job, Link, Listing, Sink, copy, read_target};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names;
use crate::source::Source;
use crate::volumes::VolumeSet;

/// High 16 bits of the attributes hold a Unix mode when this bit is set.
const UNIX_EXTENSION: u32 = 0x8000;

pub(crate) struct SevenZip {
    archive: Archive,
    source: Source,
}

fn password_of(password: Option<&[u8]>) -> Password {
    match password.map(String::from_utf8_lossy) {
        Some(password) => Password::new(&password),
        None => Password::empty(),
    }
}

pub(crate) fn open(volumes: VolumeSet, password: Option<&[u8]>) -> Result<(SevenZip, Listing)> {
    let mut source = Source::open(&volumes.paths)?;
    let archive = Archive::read(&mut source, &password_of(password))
        .map_err(|e| sevenz_error(e, password.is_some()))?;
    let members = archive
        .files
        .iter()
        .enumerate()
        .map(|(index, entry)| member(&archive, index, entry))
        .collect();
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: "7z".to_string(),
        solid: archive.is_solid,
        comment: None,
        sizes_exact: true,
        packed_size: Some(source.len()),
    };
    Ok((SevenZip { archive, source }, listing))
}

fn member(archive: &Archive, index: usize, entry: &ArchiveEntry) -> Member {
    let attributes = entry.windows_attributes;
    let mode = (entry.has_windows_attributes && attributes & UNIX_EXTENSION != 0)
        .then_some(attributes >> 16);
    let is_link = mode.is_some_and(|m| m & 0o170000 == 0o120000);
    Member {
        rel_path: names::safe_relative_path(entry.name.as_bytes(), true),
        display: entry.name.clone(),
        entry_id: None,
        kind: if entry.is_directory {
            Kind::Dir
        } else if is_link {
            Kind::Link
        } else {
            Kind::File
        },
        size: entry.size,
        packed_size: entry.compressed_size,
        mtime: entry
            .has_last_modified_date
            .then(|| SystemTime::from(entry.last_modified_date)),
        mode: mode.map(|m| m & 0o777),
        encrypted: archive.is_file_encrypted(index),
        hidden: false,
    }
}

impl SevenZip {
    /// For each block holding a wanted file, the index of the last one.
    fn wanted_blocks(&self, wanted: &[bool]) -> HashMap<usize, usize> {
        let mut blocks = HashMap::new();
        for (index, block) in self.archive.stream_map.file_block_index.iter().enumerate() {
            if let Some(block) = block
                && wanted[index]
            {
                blocks.insert(*block, index);
            }
        }
        blocks
    }

    fn extract_block(
        &self,
        block: usize,
        last: usize,
        threads: u32,
        source: &mut Source,
        job: &Job<'_>,
        sink: &dyn Sink,
    ) -> Result<()> {
        let password = password_of(job.password);
        let encrypted = job.password.is_some();
        let failure = RefCell::new(None);
        let mut index = self.archive.stream_map.block_first_file_index[block];
        let decoder = BlockDecoder::new(threads, block, &self.archive, &password, source);
        let outcome = decoder.for_each_entries(&mut |_entry, reader| {
            let current = index;
            index += 1;
            match self.decode_entry(current, reader, job, sink) {
                Ok(()) => Ok(current < last),
                Err(error) => {
                    failure.replace(Some(error));
                    Err(sevenz_rust2::Error::Other(
                        "stopped by the destination".into(),
                    ))
                }
            }
        });
        match outcome {
            Ok(_) => Ok(()),
            Err(error) => Err(failure
                .take()
                .unwrap_or_else(|| sevenz_error(error, encrypted))),
        }
    }

    fn decode_entry(
        &self,
        index: usize,
        reader: &mut dyn Read,
        job: &Job<'_>,
        sink: &dyn Sink,
    ) -> Result<()> {
        job.check_cancelled()?;
        let member = &job.members[index];
        if !job.wanted[index] {
            // Solid blocks decode through unwanted files; count them.
            return copy(reader, &mut sink.discard(), job, member.encrypted);
        }
        match member.kind {
            Kind::Dir => drop(sink.open(index)?),
            Kind::Link => {
                let target = read_target(reader, member.encrypted)?;
                sink.link(index, Link::Symlink(&target))?;
            }
            Kind::File => copy(reader, &mut sink.open(index)?, job, member.encrypted)?,
        }
        Ok(())
    }
}

impl Backend for SevenZip {
    fn planned_bytes(&self, members: &[Member], wanted: &[bool]) -> u64 {
        let blocks = self.wanted_blocks(wanted);
        let map = &self.archive.stream_map;
        (0..members.len())
            .filter(|&i| match map.file_block_index[i] {
                Some(block) => blocks.get(&block).is_some_and(|&last| i <= last),
                None => wanted[i],
            })
            .filter(|&i| members[i].kind != Kind::Dir)
            .map(|i| members[i].size)
            .sum()
    }

    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        // Folders and empty files have no data in any block.
        for (index, block) in self.archive.stream_map.file_block_index.iter().enumerate() {
            if block.is_none() && job.wanted[index] {
                match job.members[index].kind {
                    Kind::Link => sink.link(index, Link::Unsupported)?,
                    _ => drop(sink.open(index)?),
                }
            }
        }
        let mut blocks: Vec<_> = self.wanted_blocks(job.wanted).into_iter().collect();
        blocks.sort_unstable();
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        // A lone solid block can still use LZMA2's own threads.
        let threads = (cores / blocks.len().max(1)).max(1) as u32;
        blocks.par_iter().try_for_each_init(
            || self.source.clone(),
            |source, &(block, last)| self.extract_block(block, last, threads, source, job, sink),
        )
    }
}

fn sevenz_error(error: sevenz_rust2::Error, with_password: bool) -> CoreError {
    use sevenz_rust2::Error;
    let message = error.to_string();
    match error {
        Error::PasswordRequired => CoreError::PasswordRequired,
        Error::MaybeBadPassword(_) => CoreError::BadPassword,
        Error::ChecksumVerificationFailed | Error::NextHeaderCrcMismatch if with_password => {
            CoreError::BadPassword
        }
        Error::Unsupported(_)
        | Error::UnsupportedCompressionMethod(_)
        | Error::UnsupportedVersion { .. }
        | Error::ExternalUnsupported
        | Error::MaxMemLimited { .. } => CoreError::Unsupported { message },
        Error::Io(error, _) | Error::FileOpen(error, _) => super::read_error(error, with_password),
        _ => CoreError::Corrupt { message },
    }
}
