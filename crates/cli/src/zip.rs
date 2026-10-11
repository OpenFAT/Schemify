//! `zip`: project archives in and out (D31, web spec §3). The writer stores entries
//! uncompressed with a fixed timestamp, so the same files give the same bytes on every
//! platform (rule 4); the reader takes stored and deflated entries (what OS archivers and
//! forges produce) and checks every CRC.
//!
//! Data design: in, `(relative path, bytes)` rows (always read together, so AoS); out, one
//! archive. At most [`ENTRIES_MAX`] entries and 4 GiB per entry and per archive (no zip64);
//! the reader's total output is capped by its caller. Whole archives live in memory for one
//! import or export.

/// Entries per archive: the classic format's 16-bit count, whose all-ones value marks zip64
/// (neither written nor read).
pub const ENTRIES_MAX: usize = 0xFFFE;

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
/// Version 2.0 (deflate, directories); also the "made by" version.
const VERSION: u16 = 20;
/// General-purpose flag bit 11: names are UTF-8.
const UTF8: u16 = 1 << 11;
/// 1980-01-01 00:00, the DOS epoch: a fixed stamp keeps archives byte-identical.
const DOS_DATE: u16 = (1 << 5) | 1;
const LOCAL_LEN: usize = 30;
const CENTRAL_LEN: usize = 46;
const END_LEN: usize = 22;

/// The CRC-32 (IEEE 802.3, reflected, as zip uses) of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    // ponytail: bitwise, 8 steps per byte (~0.3 GB/s); a slice-by-8 table if exports of
    // large projects ever show up in a profile.
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Writes `entries` (path with `/` separators, bytes) as a stored archive, in the order
/// given.
///
/// # Errors
///
/// More than [`ENTRIES_MAX`] entries, a path longer than 65535 bytes, or an entry or archive
/// past 4 GiB.
pub fn write(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    if entries.len() > ENTRIES_MAX {
        return Err(format!("more than {ENTRIES_MAX} files"));
    }
    let size: usize = entries
        .iter()
        .map(|(p, b)| LOCAL_LEN + CENTRAL_LEN + 2 * p.len() + b.len())
        .sum::<usize>()
        + END_LEN;
    if size > u32::MAX as usize {
        return Err("archive past 4 GiB".into());
    }
    let mut out = Vec::with_capacity(size);
    let mut central = Vec::with_capacity(entries.len() * CENTRAL_LEN);
    for (path, bytes) in entries {
        let name_len = u16::try_from(path.len()).map_err(|_| format!("{path}: name too long"))?;
        let len = u32::try_from(bytes.len()).expect("archive size checked above");
        let offset = u32::try_from(out.len()).expect("archive size checked above");
        let crc = crc32(bytes);
        // Fields shared by the local and the central header, from "version needed" on.
        let mut common = Vec::with_capacity(26);
        for v in [VERSION, UTF8, 0, 0, DOS_DATE] {
            common.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, len, len] {
            common.extend_from_slice(&v.to_le_bytes());
        }
        common.extend_from_slice(&name_len.to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        out.extend_from_slice(&LOCAL.to_le_bytes());
        out.extend_from_slice(&common);
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(bytes);
        central.extend_from_slice(&CENTRAL.to_le_bytes());
        central.extend_from_slice(&VERSION.to_le_bytes()); // made by
        central.extend_from_slice(&common);
        for v in [0u16, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes()); // comment, disk, internal attributes
        }
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(path.as_bytes());
    }
    let count = u16::try_from(entries.len()).expect("checked above");
    let cd_offset = u32::try_from(out.len()).expect("archive size checked above");
    let cd_len = u32::try_from(central.len()).expect("archive size checked above");
    out.extend_from_slice(&central);
    out.extend_from_slice(&END.to_le_bytes());
    for v in [0u16, 0, count, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    assert_eq!(out.len(), size, "size precomputed exactly");
    Ok(out)
}

/// Reads every file entry of `archive` in central-directory order: `(name, bytes)`, names as
/// stored (directory entries skipped). Stored and deflated entries only; each CRC is checked.
///
/// Cost: one pass, output bounded by `bytes_max` in total (a zip bomb stops there).
///
/// # Errors
///
/// Not a zip, truncated or inconsistent headers, zip64, encryption, another compression
/// method, a CRC mismatch, or more than `bytes_max` bytes of output.
pub fn read(archive: &[u8], bytes_max: usize) -> Result<Vec<(String, Vec<u8>)>, String> {
    let end = find_end(archive).ok_or("not a zip archive (no end record)")?;
    let count = usize::from(u16_at(archive, end + 10)?);
    let cd_len = u32_at(archive, end + 12)? as usize;
    let cd_offset = u32_at(archive, end + 16)? as usize;
    if count > ENTRIES_MAX || cd_offset == u32::MAX as usize {
        return Err("zip64 archives are not supported".into());
    }
    if cd_offset.checked_add(cd_len).is_none_or(|e| e > end) {
        return Err("central directory outside the archive".into());
    }
    let mut out = Vec::with_capacity(count);
    let mut total = 0usize;
    let mut at = cd_offset;
    for _ in 0..count {
        if u32_at(archive, at)? != CENTRAL {
            return Err(format!("bad central header at {at}"));
        }
        let flags = u16_at(archive, at + 8)?;
        let method = u16_at(archive, at + 10)?;
        let crc = u32_at(archive, at + 16)?;
        let packed = u32_at(archive, at + 20)? as usize;
        let size = u32_at(archive, at + 24)? as usize;
        let name_len = usize::from(u16_at(archive, at + 28)?);
        let skip = usize::from(u16_at(archive, at + 30)?) + usize::from(u16_at(archive, at + 32)?);
        let local = u32_at(archive, at + 42)? as usize;
        let name = slice(archive, at + CENTRAL_LEN, name_len)?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += CENTRAL_LEN + name_len + skip;
        if name.ends_with('/') {
            continue; // a directory
        }
        if flags & 1 != 0 {
            return Err(format!("{name}: encrypted entries are not supported"));
        }
        if packed == u32::MAX as usize || size == u32::MAX as usize {
            return Err(format!("{name}: zip64 entries are not supported"));
        }
        total = total.saturating_add(size);
        if total > bytes_max {
            return Err(format!("more than {bytes_max} bytes uncompressed"));
        }
        if u32_at(archive, local)? != LOCAL {
            return Err(format!("{name}: bad local header"));
        }
        let data_at = local
            + LOCAL_LEN
            + usize::from(u16_at(archive, local + 26)?)
            + usize::from(u16_at(archive, local + 28)?);
        let data = slice(archive, data_at, packed)?;
        let bytes = match method {
            0 if packed == size => data.to_vec(),
            0 => return Err(format!("{name}: stored sizes differ")),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, size)
                .map_err(|e| format!("{name}: deflate: {e}"))?,
            m => return Err(format!("{name}: compression method {m} is not supported")),
        };
        if bytes.len() != size {
            return Err(format!("{name}: {} bytes, header says {size}", bytes.len()));
        }
        if crc32(&bytes) != crc {
            return Err(format!("{name}: CRC mismatch"));
        }
        out.push((name, bytes));
    }
    assert!(out.len() <= count);
    Ok(out)
}

/// Offset of the end-of-central-directory record: the last signature whose comment length
/// reaches exactly the end of the archive.
fn find_end(archive: &[u8]) -> Option<usize> {
    let last = archive.len().checked_sub(END_LEN)?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    (first..=last).rev().find(|&i| {
        archive[i..i + 4] == END.to_le_bytes()
            && i + END_LEN + usize::from(u16::from_le_bytes([archive[i + 20], archive[i + 21]]))
                == archive.len()
    })
}

fn slice(b: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
    at.checked_add(len)
        .and_then(|end| b.get(at..end))
        .ok_or_else(|| format!("truncated archive (need {len} bytes at {at})"))
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    let s = slice(b, at, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    let s = slice(b, at, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written by an independent implementation, CPython's `zipfile`: `proj/a.txt` deflated
    /// (`"hello hello hello hello\n"`), the directory `proj/sub/`, and `proj/sub/b.bin`
    /// stored (bytes 0..64), all stamped 2026-10-11.
    const PYTHON_ZIP: &str = "
    504b030414000000080000004b5d0088590b0b000000180000000a00000070726f6a2f612e747874cb48cdc9
    c957c84027b900504b030414000000000000004b5d0000000000000000000000000900000070726f6a2f7375
    622f504b030414000000000000004b5d8cce0e1040000000400000000e00000070726f6a2f7375622f622e62
    696e000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20212223242526272829
    2a2b2c2d2e2f303132333435363738393a3b3c3d3e3f504b0102140314000000080000004b5d0088590b0b00
    0000180000000a000000000000000000000080010000000070726f6a2f612e747874504b0102140314000000
    000000004b5d00000000000000000000000009000000000000000000000080013300000070726f6a2f737562
    2f504b0102140314000000000000004b5d8cce0e1040000000400000000e000000000000000000000080015a
    00000070726f6a2f7375622f622e62696e504b05060000000003000300ab000000c60000000000
    ";

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex fixture"))
            .collect()
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn reads_an_archive_written_by_python() {
        let files = read(&hex(PYTHON_ZIP), 1 << 20).expect("python's zip reads");
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["proj/a.txt", "proj/sub/b.bin"],
            "directory entry skipped"
        );
        assert_eq!(files[0].1, b"hello hello hello hello\n");
        assert_eq!(files[1].1, (0..64u8).collect::<Vec<_>>());
        // The output cap holds before anything past it is inflated.
        assert!(read(&hex(PYTHON_ZIP), 80).is_err());
    }

    #[test]
    fn write_then_read_round_trips_and_is_deterministic() {
        let big: Vec<u8> = (0..70_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let entries: Vec<(&str, &[u8])> = vec![
            ("top.shcomp", b"schemify 1 shcomp\n".as_slice()),
            ("lib/é.shprim", b"".as_slice()),
            ("data/big.bin", big.as_slice()),
        ];
        let a = write(&entries).expect("writes");
        assert_eq!(
            a,
            write(&entries).expect("writes"),
            "same input, same bytes"
        );
        let back = read(&a, usize::MAX).expect("reads its own output");
        assert_eq!(back.len(), entries.len());
        for ((n, b), (en, eb)) in back.iter().zip(&entries) {
            assert_eq!((n.as_str(), b.as_slice()), (*en, *eb));
        }
        assert_eq!(read(&write(&[]).expect("empty"), 0), Ok(Vec::new()));
    }

    #[test]
    fn damaged_archives_fail_without_panicking() {
        let a = write(&[
            ("a.txt", b"abcdef".as_slice()),
            ("b.txt", b"xyz".as_slice()),
        ])
        .expect("writes");
        for cut in 0..a.len() {
            let _ = read(&a[..cut], usize::MAX); // never panics
        }
        assert!(
            read(&a[..a.len() - 1], usize::MAX).is_err(),
            "no end record"
        );
        for i in 0..a.len() {
            let mut b = a.clone();
            b[i] ^= 0x5a;
            let _ = read(&b, usize::MAX); // never panics
        }
        let mut flipped = a.clone();
        flipped[LOCAL_LEN + 5] ^= 1; // a byte of `a.txt`'s data
        assert!(
            read(&flipped, usize::MAX).is_err_and(|e| e.contains("CRC")),
            "corruption is detected"
        );
        assert!(read(b"not a zip at all, just text", usize::MAX).is_err());
    }
}
