//! Builds small stored (uncompressed) ZIP files for tests, including ones that lie about sizes.

pub struct ZipEntry {
    name: String,
    data: Vec<u8>,
    declared_uncompressed: u64,
    zip64: bool,
}

impl ZipEntry {
    pub fn stored(name: &str, data: &[u8]) -> Self {
        Self::claiming(name, data, data.len() as u64)
    }

    /// An entry whose central directory claims `uncompressed` bytes, as a bomb does.
    pub fn claiming(name: &str, data: &[u8], uncompressed: u64) -> Self {
        Self {
            name: name.to_owned(),
            data: data.to_vec(),
            declared_uncompressed: uncompressed,
            zip64: false,
        }
    }

    /// Writes sizes and offset in the ZIP64 extra field, and the archive with ZIP64 records.
    pub fn zip64(mut self) -> Self {
        self.zip64 = true;
        self
    }
}

/// A ZIP archive of `entries`, starting with a local header (`PK\x03\x04`).
pub fn zip(entries: &[ZipEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let offset = out.len() as u64;
        let crc = crc32(&e.data);
        let (comp, uncomp) = (e.data.len() as u64, e.declared_uncompressed);
        let small = |v: u64| if e.zip64 { u32::MAX } else { v as u32 };
        let mut local_extra = Vec::new();
        let mut central_extra = Vec::new();
        if e.zip64 {
            local_extra = zip64_extra(&[uncomp, comp]);
            central_extra = zip64_extra(&[uncomp, comp, offset]);
        }
        put32(&mut out, 0x0403_4b50);
        put16(&mut out, 45);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put32(&mut out, 0);
        put32(&mut out, crc);
        put32(&mut out, small(comp));
        put32(&mut out, small(uncomp));
        put16(&mut out, e.name.len() as u16);
        put16(&mut out, local_extra.len() as u16);
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&local_extra);
        out.extend_from_slice(&e.data);

        put32(&mut central, 0x0201_4b50);
        put16(&mut central, 45);
        put16(&mut central, 45);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, 0);
        put32(&mut central, crc);
        put32(&mut central, small(comp));
        put32(&mut central, small(uncomp));
        put16(&mut central, e.name.len() as u16);
        put16(&mut central, central_extra.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, 0);
        put32(&mut central, small(offset));
        central.extend_from_slice(e.name.as_bytes());
        central.extend_from_slice(&central_extra);
    }
    let dir_offset = out.len() as u64;
    let dir_size = central.len() as u64;
    out.extend_from_slice(&central);
    let count = entries.len() as u64;
    let zip64 = entries.iter().any(|e| e.zip64);
    if zip64 {
        let record = out.len() as u64;
        put32(&mut out, 0x0606_4b50);
        out.extend_from_slice(&44u64.to_le_bytes());
        put16(&mut out, 45);
        put16(&mut out, 45);
        put32(&mut out, 0);
        put32(&mut out, 0);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&dir_size.to_le_bytes());
        out.extend_from_slice(&dir_offset.to_le_bytes());
        put32(&mut out, 0x0706_4b50);
        put32(&mut out, 0);
        out.extend_from_slice(&record.to_le_bytes());
        put32(&mut out, 1);
    }
    put32(&mut out, 0x0605_4b50);
    put16(&mut out, 0);
    put16(&mut out, 0);
    let (count16, size32, offset32) = if zip64 {
        (u16::MAX, u32::MAX, u32::MAX)
    } else {
        (count as u16, dir_size as u32, dir_offset as u32)
    };
    put16(&mut out, count16);
    put16(&mut out, count16);
    put32(&mut out, size32);
    put32(&mut out, offset32);
    put16(&mut out, 0);
    out
}

fn zip64_extra(values: &[u64]) -> Vec<u8> {
    let mut extra = Vec::new();
    put16(&mut extra, 1);
    put16(&mut extra, (values.len() * 8) as u16);
    for v in values {
        extra.extend_from_slice(&v.to_le_bytes());
    }
    extra
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
