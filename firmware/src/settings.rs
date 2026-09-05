// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! Persistent user settings (alarm + toggles) in raw flash.
//!
//! Stored as one small CRC-checked record at the start of a dedicated
//! `settings` data partition (located by label, see `partitions.csv`) — never
//! the standard `nvs` partition. A torn write fails the CRC and falls back to
//! defaults rather than loading garbage. Writes are infrequent (debounced on
//! change), so a single erased sector with no wear-levelling is fine.

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{FlashRegion, read_partition_table};
use esp_storage::FlashStorage;

/// Record tag (`"ENCS"`, little-endian) marking a valid settings blob.
const MAGIC: u32 = 0x5343_4E45;
/// Record layout version (rejected on read if it does not match).
const VERSION: u8 = 1;
/// Payload length (magic, version, flags, alarm, toggles).
const PAYLOAD_LEN: usize = 12;
/// Full record length (payload + CRC32), word-aligned.
const RECORD_LEN: usize = 16;
/// Flash sector size (erase granularity).
const SECTOR: u32 = 4096;
/// Scratch buffer for reading the partition table.
const TABLE_BUF: usize = 512;
/// Partition label we own for settings.
const LABEL: &str = "settings";

/// Persisted user settings.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// Armed alarm (12-hour minute), or `None`.
    pub alarm: Option<u16>,
    /// Toggle bitmap.
    pub toggles: u32,
}

/// CRC-32 (IEEE, reflected) over `data` — table-free.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = crc.wrapping_shr(1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Runs `op` against the `settings` partition's storage, or `None` if the
/// partition can't be located/validated.
fn with_region<R>(op: impl FnOnce(&mut FlashRegion<'_, FlashStorage>) -> R) -> Option<R> {
    let mut flash = FlashStorage::new();
    let mut buf = [0u8; TABLE_BUF];
    let table = read_partition_table(&mut flash, &mut buf).ok()?;
    let entry = table.iter().find(|e| e.label_as_str() == LABEL)?;
    if entry.is_read_only() || entry.len() < SECTOR {
        return None;
    }
    let mut region = entry.as_embedded_storage(&mut flash);
    Some(op(&mut region))
}

/// Decodes a record, or `None` if CRC, magic, or version is wrong.
fn decode(record: &[u8]) -> Option<Settings> {
    let payload = record.get(0..PAYLOAD_LEN)?;
    let stored_crc = u32::from_le_bytes(record.get(PAYLOAD_LEN..RECORD_LEN)?.try_into().ok()?);
    if crc32(payload) != stored_crc {
        return None;
    }
    if u32::from_le_bytes(payload.get(0..4)?.try_into().ok()?) != MAGIC {
        return None;
    }
    if *payload.get(4)? != VERSION {
        return None;
    }
    let flags = *payload.get(5)?;
    let alarm = if flags & 1 == 1 {
        Some(u16::from_le_bytes(payload.get(6..8)?.try_into().ok()?))
    } else {
        None
    };
    let toggles = u32::from_le_bytes(payload.get(8..12)?.try_into().ok()?);
    Some(Settings { alarm, toggles })
}

/// Serializes a settings record (payload + trailing CRC32).
fn encode(settings: &Settings) -> [u8; RECORD_LEN] {
    let [m0, m1, m2, m3] = MAGIC.to_le_bytes();
    let (flags, alarm) = match settings.alarm {
        Some(minute) => (1u8, minute),
        None => (0u8, 0),
    };
    let [a0, a1] = alarm.to_le_bytes();
    let [t0, t1, t2, t3] = settings.toggles.to_le_bytes();
    let payload = [m0, m1, m2, m3, VERSION, flags, a0, a1, t0, t1, t2, t3];
    let [c0, c1, c2, c3] = crc32(&payload).to_le_bytes();
    [
        m0, m1, m2, m3, VERSION, flags, a0, a1, t0, t1, t2, t3, c0, c1, c2, c3,
    ]
}

/// Loads persisted settings, falling back to defaults if absent/corrupt.
#[must_use]
pub fn load() -> Settings {
    with_region(|region| {
        let mut record = [0u8; RECORD_LEN];
        if ReadNorFlash::read(region, 0, &mut record).is_err() {
            return Settings::default();
        }
        decode(&record).unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Persists settings (erase one sector + write the record). Best-effort.
pub fn save(settings: &Settings) {
    let ok = with_region(|region| {
        NorFlash::erase(region, 0, SECTOR).is_ok()
            && NorFlash::write(region, 0, &encode(settings)).is_ok()
    });
    if ok != Some(true) {
        log::error!("settings: save failed");
    }
}
