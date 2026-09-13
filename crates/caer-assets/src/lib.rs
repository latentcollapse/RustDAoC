//! DAoC **MPAK** (`.mpk`) archive reader.
//!
//! Format, reverse-engineered from the real 1.127 client (see `project-daoc-gamedll-rust`):
//!
//! ```text
//!   "MPAK"  magic (4 bytes)
//!   header  (a short fixed block; we skip to the first zlib stream rather than decode it)
//!   zlib #0 -> the archive's own name (e.g. "csv001.mpk")
//!   zlib #1 -> the DIRECTORY: fixed 284-byte records, each = name[256] + 7×u32 metadata
//!              (sizes, offsets, timestamp, checksum — decoded below)
//!   zlib #2..N -> each file's contents, one zlib stream per directory entry, in order
//! ```
//!
//! Every block is an independent zlib stream, so the archive is just the magic followed by a run
//! of concatenated zlib streams. [`read`] walks them by inflating one stream at a time and
//! advancing by exactly the bytes that stream consumed — it needs the whole archive anyway, so the
//! directory metadata buys it nothing.
//!
//! The directory's metadata ints *are* decoded, though, because they give random access. Each
//! record's 7×u32 tail is:
//!
//! ```text
//!   [0] timestamp   [1] 4 (constant)      [2] offset of the member's DECOMPRESSED bytes
//!   [3] decompressed size                 [4] offset of the member's zlib stream
//!   [5] compressed size                   [6] checksum (not zlib crc32 — we don't verify it)
//! ```
//!
//! `[4]` is relative to the first file block, so one member can be fetched by inflating only its
//! own stream — see [`read_named`]. Verified against a sequential walk over every member of every
//! archive in the 1.127 retail tree — 4,999 archives, 55,372 members: byte-identical, zero
//! mismatches, and `[3]` agreed with the inflated length every time. `tests/mpak_random_access.rs`
//! re-runs that comparison over a sample.

use std::io;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use flate2::{Decompress, FlushDecompress, Status};

pub mod anims;
pub mod bitmapfont;
pub mod bmp;
pub mod client_dep;
pub mod clientini;
pub mod dds;
pub mod descriptions;
pub mod dungeon;
pub mod fig3_look;
pub mod figures;
pub mod fixtures;
pub mod heightmap;
pub mod monsters;
pub mod mskins;
pub mod names;
pub mod nhd;
pub mod nif;
pub mod pcx;
pub mod pskins;
pub mod sector;
pub mod sky;
pub mod soundmap;
pub mod terrain;
pub mod tga;
pub mod uiskin;
pub mod zonesounds;

/// Bytes per directory record: `name[256]` followed by 7×`u32` of metadata.
const DIR_RECORD: usize = 284;

/// One extracted archive member.
#[derive(Debug, Clone)]
pub struct MpakEntry {
    /// The member's file name, as recorded in the directory (e.g. `"nifs.csv"`).
    pub name: String,
    /// The member's decompressed contents.
    pub data: Vec<u8>,
}

/// One member's directory record: where it lives, not what it holds.
#[derive(Debug, Clone)]
pub struct MpakMemberInfo {
    pub name: String,
    /// Decompressed length in bytes.
    pub size: u32,
    /// Offset of the member's zlib stream, relative to the first file block.
    pub offset: u32,
    /// Length of that zlib stream.
    pub compressed_size: u32,
}

/// Read and fully decompress an MPAK archive from a file.
pub fn open(path: impl AsRef<Path>) -> io::Result<Vec<MpakEntry>> {
    read(&std::fs::read(path)?)
}

/// Fetch specific members from an archive file, reading and inflating only those members.
///
/// Prefer this over [`open`] whenever the caller wants part of an archive. `pregame.mpk` holds 33
/// members and 22 MB of art; pulling `realmdesc.txt` out of it with `open` costs ~177 ms of CPU
/// plus a 22 MB read, and with this it costs the directory plus that member's own bytes.
pub fn open_named(path: impl AsRef<Path>, wanted: &[&str]) -> io::Result<Vec<MpakEntry>> {
    let mut file = std::fs::File::open(path)?;
    let (members, data_start) = read_directory(&mut file)?;

    let mut out = Vec::with_capacity(wanted.len());
    for name in wanted {
        let Some(m) = members.iter().find(|m| m.name.eq_ignore_ascii_case(name)) else {
            continue;
        };
        out.push(fetch(&mut file, data_start, m)?);
    }
    Ok(out)
}

/// Fetch the first member whose name satisfies `matches` — for archives addressed by kind rather
/// than by name, such as a model `.npk` whose one `.nif` sits among its textures.
pub fn open_first(
    path: impl AsRef<Path>,
    matches: impl Fn(&str) -> bool,
) -> io::Result<Option<MpakEntry>> {
    let mut file = std::fs::File::open(path)?;
    let (members, data_start) = read_directory(&mut file)?;
    match members.iter().find(|m| matches(&m.name)) {
        Some(m) => Ok(Some(fetch(&mut file, data_start, m)?)),
        None => Ok(None),
    }
}

/// Read one member's zlib stream off disk and inflate it — nothing else in the archive is touched.
fn fetch(file: &mut std::fs::File, data_start: usize, m: &MpakMemberInfo) -> io::Result<MpakEntry> {
    let mut buf = vec![0u8; m.compressed_size as usize];
    file.seek(SeekFrom::Start((data_start + m.offset as usize) as u64))?;
    file.read_exact(&mut buf).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{}: reading {} bytes at {}: {e}",
                m.name,
                buf.len(),
                m.offset
            ),
        )
    })?;
    Ok(MpakEntry {
        name: m.name.clone(),
        data: inflate_member(m, &buf)?,
    })
}

/// Read just the header and directory off an open archive, without pulling in the file bodies.
///
/// The directory is a zlib stream of unknown compressed length, so this reads a window from the
/// front of the file and grows it until the stream ends cleanly. Real archives fit the first
/// window; the loop exists so an unusually large directory cannot be silently truncated into a
/// short member list.
fn read_directory(file: &mut std::fs::File) -> io::Result<(Vec<MpakMemberInfo>, usize)> {
    const FIRST_WINDOW: usize = 128 * 1024;
    let len = file.metadata()?.len() as usize;
    let mut window = FIRST_WINDOW.min(len).max(6);
    loop {
        let mut head = vec![0u8; window.min(len)];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut head)?;
        match directory(&head) {
            Ok(dir) => return Ok(dir),
            // A window that ends mid-directory is not a corrupt archive — grow and retry.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && window < len => {
                window = (window * 4).min(len);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Fetch a single member by name. `Ok(None)` means the archive parsed but has no such member.
pub fn open_member(path: impl AsRef<Path>, name: &str) -> io::Result<Option<Vec<u8>>> {
    Ok(open_named(path, &[name])?.pop().map(|e| e.data))
}

/// Fetch specific members from an in-memory MPAK, inflating only those members.
///
/// Names match case-insensitively, because the client's own references disagree with the directory
/// on case (`banner_albion.DDS` is asked for as `.dds`). Results come back in `wanted` order;
/// members the archive does not carry are simply absent, so a caller that needs to report a miss
/// should compare lengths or look up by name.
pub fn read_named(bytes: &[u8], wanted: &[&str]) -> io::Result<Vec<MpakEntry>> {
    let (members, data_start) = directory(bytes)?;
    let mut out = Vec::with_capacity(wanted.len());
    for name in wanted {
        let Some(m) = members.iter().find(|m| m.name.eq_ignore_ascii_case(name)) else {
            continue;
        };
        let start = data_start + m.offset as usize;
        let end = start + m.compressed_size as usize;
        if end > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "directory places {} at {start}..{end}, past the {}-byte archive",
                    m.name,
                    bytes.len()
                ),
            ));
        }
        out.push(MpakEntry {
            name: m.name.clone(),
            data: inflate_member(m, &bytes[start..end])?,
        });
    }
    Ok(out)
}

/// Parse the directory. Returns the member records and the offset of the first file block, which
/// is what member offsets are relative to.
pub fn directory(bytes: &[u8]) -> io::Result<(Vec<MpakMemberInfo>, usize)> {
    check_magic(bytes)?;
    let mut pos = first_zlib_offset(bytes, 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no zlib stream found"))?;
    // block #0: archive self-name — discard.
    pos += inflate_one(&bytes[pos..])?.1;
    // block #1: directory.
    let (dir, consumed) = inflate_one(&bytes[pos..])?;
    Ok((parse_directory(&dir), pos + consumed))
}

/// List member names without reading or decompressing file bodies — for coverage/inventory gates
/// and for indexes that need to know what is in an archive but not what it looks like.
pub fn list_names(path: impl AsRef<Path>) -> io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    Ok(read_directory(&mut file)?
        .0
        .into_iter()
        .map(|m| m.name)
        .collect())
}

/// List member names from an in-memory MPAK.
pub fn list_names_bytes(bytes: &[u8]) -> io::Result<Vec<String>> {
    Ok(directory(bytes)?.0.into_iter().map(|m| m.name).collect())
}

/// Read and fully decompress an MPAK archive from bytes already in memory.
pub fn read(bytes: &[u8]) -> io::Result<Vec<MpakEntry>> {
    let (members, mut pos) = directory(bytes)?;

    // blocks #2..N: one file per directory entry, in order.
    let mut entries = Vec::with_capacity(members.len());
    for m in members {
        if pos >= bytes.len() {
            break;
        }
        let (data, consumed) = inflate_one(&bytes[pos..])?;
        pos += consumed;
        entries.push(MpakEntry { name: m.name, data });
    }
    Ok(entries)
}

/// Inflate one member's stream and hold it to the length its directory record promises. A stream
/// that inflates to a different size means the offsets were misread, so it is an error rather than
/// a shorter texture.
fn inflate_member(m: &MpakMemberInfo, stream: &[u8]) -> io::Result<Vec<u8>> {
    let (data, _) = inflate_one(stream)?;
    if data.len() != m.size as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} inflated to {} bytes, directory says {}",
                m.name,
                data.len(),
                m.size
            ),
        ));
    }
    Ok(data)
}

fn check_magic(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() < 6 || &bytes[0..4] != b"MPAK" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an MPAK archive (bad magic)",
        ));
    }
    Ok(())
}

/// Parse the directory block. Each record is `DIR_RECORD` bytes: a NUL-terminated name in the
/// first 256, then the 7×u32 metadata tail described at the top of this module.
fn parse_directory(dir: &[u8]) -> Vec<MpakMemberInfo> {
    dir.chunks_exact(DIR_RECORD)
        .map(|rec| {
            let name_field = &rec[..256];
            let end = name_field.iter().position(|&b| b == 0).unwrap_or(256);
            let int = |i: usize| {
                let at = 256 + i * 4;
                u32::from_le_bytes(rec[at..at + 4].try_into().unwrap())
            };
            MpakMemberInfo {
                name: String::from_utf8_lossy(&name_field[..end]).into_owned(),
                size: int(3),
                offset: int(4),
                compressed_size: int(5),
            }
        })
        .collect()
}

/// Index of the first zlib stream (`0x78` + a valid FLG byte) at or after `from`.
fn first_zlib_offset(bytes: &[u8], from: usize) -> Option<usize> {
    (from..bytes.len().saturating_sub(1)).find(|&i| {
        // 0x78 = CMF for 32K-window deflate; the common FLG bytes are 0x01/0x9c/0xda.
        bytes[i] == 0x78 && matches!(bytes[i + 1], 0x01 | 0x5e | 0x9c | 0xda)
    })
}

/// Inflate exactly one zlib stream from the front of `input`. Returns the decompressed bytes and
/// the number of input bytes the stream consumed (so the caller can advance to the next stream).
///
/// Running out of input before the stream ends is `UnexpectedEof`, not a short result. Callers use
/// that to tell "this archive is truncated" from "my read window was too small" — silently
/// returning half a member instead would surface as a corrupt texture with no trail back here.
fn inflate_one(input: &[u8]) -> io::Result<(Vec<u8>, usize)> {
    let mut d = Decompress::new(/* zlib_header = */ true);
    let mut out = Vec::new();
    let mut scratch = [0u8; 64 * 1024];
    loop {
        let before_in = d.total_in();
        let before_out = d.total_out();
        let in_off = before_in as usize;
        let status = d
            .decompress(&input[in_off..], &mut scratch, FlushDecompress::None)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let after_in = d.total_in();
        let after_out = d.total_out();
        let produced = after_out as usize - before_out as usize;
        out.extend_from_slice(&scratch[..produced]);
        ensure_inflate_progress(status, before_in, after_in, before_out, after_out)?;
        match status {
            Status::StreamEnd => break,
            // No progress possible: the stream needs input we don't have.
            Status::BufError => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "zlib stream ended early: {} of {} input bytes consumed, {} produced",
                        d.total_in(),
                        input.len(),
                        out.len()
                    ),
                ))
            }
            Status::Ok => {}
        }
    }
    Ok((out, d.total_in() as usize))
}

/// A zlib decoder is allowed to return `Ok` while it needs another call, but it must advance its
/// input cursor or emit bytes.  Without this guard a malformed directory stream can return `Ok`
/// forever, pinning an inspection tool on one CPU core rather than reporting a corrupt archive.
fn ensure_inflate_progress(
    status: Status,
    before_in: u64,
    after_in: u64,
    before_out: u64,
    after_out: u64,
) -> io::Result<()> {
    if matches!(status, Status::Ok) && before_in == after_in && before_out == after_out {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zlib stream made no progress",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zlib_ok_without_any_consumption_or_output_is_refused() {
        let error = ensure_inflate_progress(Status::Ok, 42, 42, 9, 9)
            .expect_err("a non-progressing decoder would otherwise spin forever");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("no progress"));
    }

    #[test]
    fn zlib_ok_that_advances_is_allowed() {
        ensure_inflate_progress(Status::Ok, 42, 43, 9, 9)
            .expect("input consumption is real progress");
        ensure_inflate_progress(Status::Ok, 42, 42, 9, 10)
            .expect("output production is real progress");
    }
}
