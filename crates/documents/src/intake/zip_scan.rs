//! ZIP bomb guard: reads the central directory at the end of the file, never decompresses.
use super::Rejection;

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD_LEN: usize = 22;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const ZIP64_LOCATOR_LEN: usize = 20;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const ZIP64_EOCD_LEN: usize = 56;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const CENTRAL_LEN: usize = 46;
const MAX_COMMENT: usize = 0xffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZipLimits {
    pub max_entries: u64,
    /// Declared uncompressed size of all entries together.
    pub max_uncompressed_bytes: u64,
    pub max_central_directory_bytes: u64,
}

impl ZipLimits {
    /// Far above any real document (a large deck has a few thousand parts), far below what an
    /// editor in the browser could unpack.
    pub const DEFAULT: Self = Self {
        max_entries: 10_000,
        max_uncompressed_bytes: 2 * 1024 * 1024 * 1024,
        max_central_directory_bytes: 4 * 1024 * 1024,
    };

    /// How many trailing bytes must be kept to scan a file within these limits.
    pub fn tail_bytes(&self) -> usize {
        let directory = usize::try_from(self.max_central_directory_bytes).unwrap_or(usize::MAX);
        directory.saturating_add(ZIP64_EOCD_LEN + ZIP64_LOCATOR_LEN + EOCD_LEN + MAX_COMMENT)
    }
}

/// Totals declared by the central directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZipSummary {
    pub entries: u64,
    pub uncompressed_bytes: u64,
}

struct Directory {
    entries: u64,
    size: u64,
    offset: u64,
    /// Where the record that describes the directory starts; the directory ends before it.
    end: u64,
}

/// Scans `tail`, the last bytes of a file of `file_len` bytes.
pub fn scan(tail: &[u8], file_len: u64, limits: &ZipLimits) -> Result<ZipSummary, Rejection> {
    let tail_start = file_len
        .checked_sub(tail.len() as u64)
        .ok_or(Rejection::InvalidSignature)?;
    let eocd = find_eocd(tail).ok_or(Rejection::InvalidSignature)?;
    let dir = directory(tail, tail_start, eocd)?;
    if dir.entries > limits.max_entries || dir.size > limits.max_central_directory_bytes {
        return Err(Rejection::Bomb);
    }
    let dir_end = dir
        .offset
        .checked_add(dir.size)
        .ok_or(Rejection::InvalidSignature)?;
    if dir.offset < tail_start || dir_end > dir.end {
        return Err(Rejection::InvalidSignature);
    }
    let start = to_index(dir.offset - tail_start)?;
    let bytes = &tail[start..start + to_index(dir.size)?];
    let summary = entries(bytes, dir.entries, dir.offset, limits)?;
    Ok(summary)
}

fn find_eocd(tail: &[u8]) -> Option<usize> {
    let last = tail.len().checked_sub(EOCD_LEN)?;
    let first = last.saturating_sub(MAX_COMMENT);
    (first..=last).rev().find(|&at| {
        read_u32(tail, at) == Some(EOCD_SIG)
            && read_u16(tail, at + 20).is_some_and(|c| at + EOCD_LEN + c as usize <= tail.len())
    })
}

fn directory(tail: &[u8], tail_start: u64, eocd: usize) -> Result<Directory, Rejection> {
    let field16 = |off| read_u16(tail, eocd + off).ok_or(Rejection::InvalidSignature);
    let field32 = |off| read_u32(tail, eocd + off).ok_or(Rejection::InvalidSignature);
    let (disk, dir_disk) = (field16(4)?, field16(6)?);
    let entries = field16(10)?;
    let (size, offset) = (field32(12)?, field32(16)?);
    let eocd_abs = tail_start + eocd as u64;
    if entries != u16::MAX && size != u32::MAX && offset != u32::MAX {
        if disk != 0 || dir_disk != 0 {
            return Err(Rejection::InvalidSignature);
        }
        return Ok(Directory {
            entries: entries.into(),
            size: size.into(),
            offset: offset.into(),
            end: eocd_abs,
        });
    }
    let locator = eocd
        .checked_sub(ZIP64_LOCATOR_LEN)
        .filter(|&at| read_u32(tail, at) == Some(ZIP64_LOCATOR_SIG))
        .ok_or(Rejection::InvalidSignature)?;
    let record_abs = read_u64(tail, locator + 8).ok_or(Rejection::InvalidSignature)?;
    let record = record_abs
        .checked_sub(tail_start)
        .and_then(|r| usize::try_from(r).ok())
        .filter(|&r| r + ZIP64_EOCD_LEN <= locator)
        .filter(|&r| read_u32(tail, r) == Some(ZIP64_EOCD_SIG))
        .ok_or(Rejection::InvalidSignature)?;
    let field64 = |off| read_u64(tail, record + off).ok_or(Rejection::InvalidSignature);
    let disks = read_u32(tail, record + 16).zip(read_u32(tail, record + 20));
    if disks != Some((0, 0)) {
        return Err(Rejection::InvalidSignature);
    }
    Ok(Directory {
        entries: field64(32)?,
        size: field64(40)?,
        offset: field64(48)?,
        end: record_abs,
    })
}

fn entries(
    dir: &[u8],
    declared: u64,
    dir_offset: u64,
    limits: &ZipLimits,
) -> Result<ZipSummary, Rejection> {
    let (mut at, mut count, mut uncompressed, mut compressed) = (0usize, 0u64, 0u64, 0u64);
    while at < dir.len() {
        if read_u32(dir, at) != Some(CENTRAL_SIG) || at + CENTRAL_LEN > dir.len() {
            return Err(Rejection::InvalidSignature);
        }
        let field16 = |off| read_u16(dir, at + off).map_or(0, usize::from);
        let field32 = |off| read_u32(dir, at + off).unwrap_or(0);
        let (name_len, extra_len, comment_len) = (field16(28), field16(30), field16(32));
        let extra_start = at + CENTRAL_LEN + name_len;
        let next = extra_start + extra_len + comment_len;
        if next > dir.len() {
            return Err(Rejection::InvalidSignature);
        }
        let mut sizes = Zip64Fields::new(&dir[extra_start..extra_start + extra_len]);
        let entry_uncompressed = sizes.resolve(field32(24))?;
        let entry_compressed = sizes.resolve(field32(20))?;
        let local_offset = sizes.resolve(field32(42))?;
        if local_offset >= dir_offset {
            return Err(Rejection::InvalidSignature);
        }
        count += 1;
        uncompressed = uncompressed.saturating_add(entry_uncompressed);
        compressed = compressed.saturating_add(entry_compressed);
        if count > limits.max_entries || uncompressed > limits.max_uncompressed_bytes {
            return Err(Rejection::Bomb);
        }
        at = next;
    }
    if count != declared {
        return Err(Rejection::InvalidSignature);
    }
    // Entries whose data overlap (the non-recursive bomb trick) claim more data than the file has.
    if compressed > dir_offset {
        return Err(Rejection::Bomb);
    }
    Ok(ZipSummary {
        entries: count,
        uncompressed_bytes: uncompressed,
    })
}

/// The ZIP64 extended information extra field (id 1): 64-bit values, in header order, for the
/// header fields set to `0xffffffff`.
struct Zip64Fields<'a> {
    values: Option<&'a [u8]>,
    used: usize,
}

impl<'a> Zip64Fields<'a> {
    fn new(mut extra: &'a [u8]) -> Self {
        while extra.len() >= 4 {
            let id = u16::from_le_bytes([extra[0], extra[1]]);
            let len = usize::from(u16::from_le_bytes([extra[2], extra[3]]));
            let Some(data) = extra.get(4..4 + len) else {
                break;
            };
            if id == 1 {
                return Self {
                    values: Some(data),
                    used: 0,
                };
            }
            extra = &extra[4 + len..];
        }
        Self {
            values: None,
            used: 0,
        }
    }

    fn resolve(&mut self, header: u32) -> Result<u64, Rejection> {
        if header != u32::MAX {
            return Ok(header.into());
        }
        let value = self
            .values
            .and_then(|v| read_u64(v, self.used))
            .ok_or(Rejection::InvalidSignature)?;
        self.used += 8;
        Ok(value)
    }
}

fn to_index(n: u64) -> Result<usize, Rejection> {
    usize::try_from(n).map_err(|_| Rejection::InvalidSignature)
}

fn read_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intake::testing::{ZipEntry, zip};

    fn check(bytes: &[u8]) -> Result<ZipSummary, Rejection> {
        scan(bytes, bytes.len() as u64, &ZipLimits::DEFAULT)
    }

    #[test]
    fn counts_entries_and_declared_sizes() {
        let file = zip(&[
            ZipEntry::stored("a.xml", b"hello"),
            ZipEntry::stored("b", b""),
        ]);
        assert_eq!(
            check(&file),
            Ok(ZipSummary {
                entries: 2,
                uncompressed_bytes: 5
            })
        );
    }

    #[test]
    fn scans_only_the_tail() {
        let file = zip(&[ZipEntry::stored("big", &vec![7u8; 100_000])]);
        let tail = &file[file.len() - 200..];
        assert_eq!(
            scan(tail, file.len() as u64, &ZipLimits::DEFAULT).map(|s| s.entries),
            Ok(1)
        );
    }

    #[test]
    fn huge_declared_size_is_a_bomb() {
        let file = zip(&[ZipEntry::claiming("x", b"tiny", 3 * 1024 * 1024 * 1024)]);
        assert_eq!(check(&file), Err(Rejection::Bomb));
    }

    #[test]
    fn too_many_entries_is_a_bomb() {
        let limits = ZipLimits {
            max_entries: 3,
            ..ZipLimits::DEFAULT
        };
        let entries: Vec<ZipEntry> = (0..4)
            .map(|i| ZipEntry::stored(&format!("{i}"), b""))
            .collect();
        let file = zip(&entries);
        assert_eq!(
            scan(&file, file.len() as u64, &limits),
            Err(Rejection::Bomb)
        );
    }

    #[test]
    fn zip64_sizes_are_read_from_the_extra_field() {
        let file = zip(&[ZipEntry::claiming("x", b"tiny", 3 * 1024 * 1024 * 1024).zip64()]);
        assert_eq!(check(&file), Err(Rejection::Bomb));
        let small = zip(&[ZipEntry::stored("x", b"tiny").zip64()]);
        assert_eq!(check(&small).map(|s| s.uncompressed_bytes), Ok(4));
    }

    #[test]
    fn truncated_or_foreign_files_are_refused() {
        let file = zip(&[ZipEntry::stored("a", b"hello")]);
        assert_eq!(
            check(&file[..file.len() / 2]),
            Err(Rejection::InvalidSignature)
        );
        assert_eq!(
            check(b"PK\x03\x04 not really"),
            Err(Rejection::InvalidSignature)
        );
        assert_eq!(check(b""), Err(Rejection::InvalidSignature));
    }
}
