// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Minimal ZIP reader/writer used by `.backupall` / `.restoreall`.
//!
//! The Python userbot stores full backups as a regular ZIP archive with
//! `db.json` and `mods.zip` inside. We keep the same container format so
//! backups can be moved between the two implementations. Entries written by
//! this module are stored (method 0); when reading we also support deflated
//! entries (method 8), which is what Python's `zipfile` produces.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{bail, Context, Result};
use flate2::read::DeflateDecoder;

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const END_SIGNATURE: u32 = 0x0605_4b50;
const MAX_COMMENT: usize = 65_535;

/// Read every file from a ZIP archive into a map keyed by entry name.
pub fn read_zip(data: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let eocd = find_end(data).context("not a ZIP archive")?;
    let count = u16_at(data, eocd + 10)? as usize;
    let central_offset = u32_at(data, eocd + 16)? as usize;
    let mut position = central_offset;
    let mut files = BTreeMap::new();

    for _ in 0..count {
        if u32_at(data, position)? != CENTRAL_SIGNATURE {
            bail!("corrupt ZIP central directory");
        }
        let method = u16_at(data, position + 10)?;
        let compressed_size = u32_at(data, position + 20)? as usize;
        let name_len = u16_at(data, position + 28)? as usize;
        let extra_len = u16_at(data, position + 30)? as usize;
        let comment_len = u16_at(data, position + 32)? as usize;
        let local_offset = u32_at(data, position + 42)? as usize;

        let name_bytes = data
            .get(position + 46..position + 46 + name_len)
            .context("truncated ZIP entry name")?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();

        let local_name_len = u16_at(data, local_offset + 26)? as usize;
        let local_extra_len = u16_at(data, local_offset + 28)? as usize;
        let data_start = local_offset + 30 + local_name_len + local_extra_len;
        let compressed = data
            .get(data_start..data_start + compressed_size)
            .context("truncated ZIP entry data")?;

        let content = match method {
            0 => compressed.to_vec(),
            8 => {
                let mut decoded = Vec::new();
                DeflateDecoder::new(compressed)
                    .read_to_end(&mut decoded)
                    .context("failed to inflate a ZIP entry")?;
                decoded
            }
            other => bail!("unsupported ZIP compression method {other}"),
        };

        files.insert(name, content);
        position += 46 + name_len + extra_len + comment_len;
    }

    Ok(files)
}

/// Build a ZIP archive with stored (uncompressed) entries.
pub fn write_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut central = Vec::new();

    for (name, content) in entries {
        let name_bytes = name.as_bytes();
        let offset = archive.len() as u32;
        let crc = crc32(content);
        let size = content.len() as u32;

        push_u32(&mut archive, LOCAL_SIGNATURE);
        push_u16(&mut archive, 20); // version needed to extract
        push_u16(&mut archive, 0x0800); // UTF-8 names
        push_u16(&mut archive, 0); // stored
        push_u16(&mut archive, 0); // DOS time
        push_u16(&mut archive, 0x0021); // DOS date: 1980-01-01
        push_u32(&mut archive, crc);
        push_u32(&mut archive, size);
        push_u32(&mut archive, size);
        push_u16(&mut archive, name_bytes.len() as u16);
        push_u16(&mut archive, 0); // extra length
        archive.extend_from_slice(name_bytes);
        archive.extend_from_slice(content);

        push_u32(&mut central, CENTRAL_SIGNATURE);
        push_u16(&mut central, 0x0314); // made by: Unix, ZIP 2.0
        push_u16(&mut central, 20); // version needed
        push_u16(&mut central, 0x0800);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0x0021);
        push_u32(&mut central, crc);
        push_u32(&mut central, size);
        push_u32(&mut central, size);
        push_u16(&mut central, name_bytes.len() as u16);
        push_u16(&mut central, 0); // extra
        push_u16(&mut central, 0); // comment
        push_u16(&mut central, 0); // disk
        push_u16(&mut central, 0); // internal attributes
        push_u32(&mut central, 0); // external attributes
        push_u32(&mut central, offset);
        central.extend_from_slice(name_bytes);
    }

    let central_offset = archive.len() as u32;
    let central_size = central.len() as u32;
    archive.extend_from_slice(&central);

    push_u32(&mut archive, END_SIGNATURE);
    push_u16(&mut archive, 0); // disk
    push_u16(&mut archive, 0); // central directory start disk
    push_u16(&mut archive, entries.len() as u16);
    push_u16(&mut archive, entries.len() as u16);
    push_u32(&mut archive, central_size);
    push_u32(&mut archive, central_offset);
    push_u16(&mut archive, 0); // comment length

    archive
}

fn find_end(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let start = data.len().saturating_sub(22 + MAX_COMMENT);
    (start..=data.len() - 22).rev().find(|position| {
        u32::from_le_bytes([
            data[*position],
            data[*position + 1],
            data[*position + 2],
            data[*position + 3],
        ]) == END_SIGNATURE
    })
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .context("truncated ZIP header")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .context("truncated ZIP header")?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn push_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_roundtrip() {
        let entries = vec![
            ("db.json".to_owned(), b"{\"a\":1}".to_vec()),
            ("modules/test.py".to_owned(), b"print(1)".to_vec()),
        ];
        let archive = write_zip(&entries);
        let restored = read_zip(&archive).expect("archive should parse");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored["db.json"], b"{\"a\":1}");
        assert_eq!(restored["modules/test.py"], b"print(1)");
    }

    #[test]
    fn crc_matches_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
