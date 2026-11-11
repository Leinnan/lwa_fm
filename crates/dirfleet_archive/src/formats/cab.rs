//! Microsoft cabinet files (`.cab`) through the cab crate: stored, MSZIP
//! and LZX folders. Each worker opens its own cabinet, so files decode in
//! parallel.

use rayon::prelude::*;

use super::{Backend, Job, Listing, Sink, copy, local_time, read_error};
use crate::archive::{Kind, Member};
use crate::error::{CoreError, Result};
use crate::names;
use crate::source::Source;
use crate::volumes::VolumeSet;

pub(crate) const MAGIC: &[u8] = b"MSCF";

pub(crate) struct Cab {
    source: Source,
    /// Names as the cabinet stores them, to read members by.
    names: Vec<String>,
}

pub(crate) fn open(volumes: VolumeSet) -> Result<(Cab, Listing)> {
    let source = Source::open(&volumes.paths)?;
    let cabinet = cab::Cabinet::new(source.clone()).map_err(|e| read_error(e, false))?;
    let mut members = Vec::new();
    let mut stored = Vec::new();
    for folder in cabinet.folder_entries() {
        for file in folder.file_entries() {
            let name = file.name().to_string();
            let mtime = file.datetime().and_then(|t| {
                local_time(
                    t.year(),
                    u8::from(t.month()),
                    t.day(),
                    t.hour(),
                    t.minute(),
                    t.second(),
                )
            });
            members.push(Member {
                // Cabinets come from Windows: backslashes separate folders.
                rel_path: names::safe_relative_path(name.as_bytes(), true),
                display: name.replace('\\', "/"),
                entry_id: None,
                kind: Kind::File,
                size: u64::from(file.uncompressed_size()),
                packed_size: 0,
                mtime,
                mode: Some(if file.is_read_only() { 0o444 } else { 0o644 }),
                encrypted: false,
                hidden: false,
            });
            stored.push(name);
        }
    }
    let listing = Listing {
        members,
        volumes: volumes.paths,
        format: "Cabinet".to_string(),
        solid: true,
        comment: None,
        sizes_exact: true,
        packed_size: Some(source.len()),
    };
    Ok((
        Cab {
            source,
            names: stored,
        },
        listing,
    ))
}

impl Backend for Cab {
    fn extract(&self, job: &Job<'_>, sink: &dyn Sink) -> Result<()> {
        let wanted: Vec<usize> = (0..self.names.len()).filter(|&i| job.wanted[i]).collect();
        wanted.par_iter().try_for_each_init(
            || cab::Cabinet::new(self.source.clone()),
            |cabinet, &index| {
                job.check_cancelled()?;
                let cabinet = cabinet.as_mut().map_err(|e| CoreError::Corrupt {
                    message: e.to_string(),
                })?;
                let mut reader = cabinet
                    .read_file(&self.names[index])
                    .map_err(|e| read_error(e, false))?;
                copy(&mut reader, &mut sink.open(index)?, job, false)
            },
        )
    }
}
