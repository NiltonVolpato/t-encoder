// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Flash key-value settings storage backed by `esp-storage` and `sequential-storage`.
//!
//! Stores user configuration (such as display brightness) in the dedicated
//! 16 KiB `settings` region at the top of 16MB flash (`0x00FF_C000`..`0x0100_0000`).

use core::cell::RefCell;
use core::ops::Range;

use defmt::{error, info, warn};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash as SyncMultiwriteNorFlash, NorFlash as SyncNorFlash,
    ReadNorFlash as SyncReadNorFlash,
};
use embedded_storage_async::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use sequential_storage::cache::Cache;
use sequential_storage::cache::key_pointers::ArrayKeyPointers;
use sequential_storage::cache::page_pointers::ArrayPagePointers;
use sequential_storage::cache::page_states::ArrayPageStates;
use sequential_storage::map::{MapConfig, MapStorage, PostcardValue};

/// Top 16 KiB reserved settings partition on 16MB flash.
const SETTINGS_RANGE: Range<u32> = 0x00FF_C000..0x0100_0000;

/// Application settings identifier key (up to 16 ASCII bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppKey(pub [u8; 16]);

impl AppKey {
    /// Creates an `AppKey` from a string slice, padding with zeros up to 16 bytes.
    #[must_use]
    pub const fn from_str(name: &str) -> Self {
        let bytes = name.as_bytes();
        let mut buffer = [0u8; 16];
        let length = if bytes.len() > 16 { 16 } else { bytes.len() };
        let mut index = 0;
        while index < length {
            buffer[index] = bytes[index];
            index += 1;
        }
        Self(buffer)
    }

    /// Returns the string representation of the key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let length = self.0.iter().position(|&byte| byte == 0).unwrap_or(self.0.len());
        let slice = self.0.get(..length).unwrap_or(&self.0);
        core::str::from_utf8(slice).unwrap_or("<invalid-utf8>")
    }
}

impl sequential_storage::map::Key for AppKey {
    fn serialize_into(
        &self,
        buffer: &mut [u8],
    ) -> Result<usize, sequential_storage::map::SerializationError> {
        self.0.serialize_into(buffer)
    }

    fn deserialize_from(
        buffer: &[u8],
    ) -> Result<(Self, usize), sequential_storage::map::SerializationError> {
        let (bytes, length) = <[u8; 16] as sequential_storage::map::Key>::deserialize_from(buffer)?;
        Ok((Self(bytes), length))
    }
}

/// Key for system-level settings.
pub const KEY_SYSTEM: AppKey = AppKey::from_str("system");

/// Adapter bridging synchronous `embedded-storage` to asynchronous `embedded-storage-async`.
pub struct BlockingAsync<T>(pub T);

impl<T: ErrorType> embedded_storage_async::nor_flash::ErrorType for BlockingAsync<T> {
    type Error = T::Error;
}

impl<T: SyncReadNorFlash> ReadNorFlash for BlockingAsync<T> {
    const READ_SIZE: usize = T::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<T: SyncNorFlash> NorFlash for BlockingAsync<T> {
    const WRITE_SIZE: usize = T::WRITE_SIZE;
    const ERASE_SIZE: usize = T::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.0.erase(from, to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.write(offset, bytes)
    }
}

impl<T: SyncMultiwriteNorFlash> MultiwriteNorFlash for BlockingAsync<T> {}

/// Persisted system configuration.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SystemSettings {
    pub brightness_percent: u8,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self { brightness_percent: 80 }
    }
}

impl PostcardValue<'_> for SystemSettings {}

/// In-memory cached system settings for instant, lock-free read access across tasks.
static CACHED_SETTINGS: BlockingMutex<CriticalSectionRawMutex, RefCell<SystemSettings>> =
    BlockingMutex::new(RefCell::new(SystemSettings { brightness_percent: 80 }));

/// Hardware flash access mutex preventing concurrent collision.
static FLASH_MUTEX: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());

/// Initializes flash storage and loads persisted settings, storing defaults if flash is empty.
pub async fn init() -> SystemSettings {
    let _guard = FLASH_MUTEX.lock().await;
    let flash = BlockingAsync(FlashStorage::new());

    let mut map_storage = MapStorage::new(
        flash,
        const { MapConfig::new(SETTINGS_RANGE) },
        Cache::new(
            ArrayPageStates::<4>::new(),
            ArrayPagePointers::<4>::new(),
            ArrayKeyPointers::<AppKey, 4>::new(),
        ),
    );

    let mut buffer = [0u8; 512];
    let loaded: SystemSettings = match map_storage
        .fetch_item::<SystemSettings>(&mut buffer, &KEY_SYSTEM)
        .await
    {
        Ok(Some(stored)) => {
            let clamped = theme::clamp_brightness(stored.brightness_percent as i32) as u8;
            info!("[STORAGE] Loaded system settings: brightness={}%", clamped);
            SystemSettings { brightness_percent: clamped }
        }
        Ok(None) => {
            info!("[STORAGE] No settings found in flash, writing defaults (80%)");
            let default_settings = SystemSettings::default();
            if let Err(e) =
                map_storage.store_item(&mut buffer, &KEY_SYSTEM, &default_settings).await
            {
                error!("[STORAGE] Failed to write default settings: {:?}", defmt::Debug2Format(&e));
            }
            default_settings
        }
        Err(e) => {
            warn!(
                "[STORAGE] Failed to read settings ({:?}), healing with defaults",
                defmt::Debug2Format(&e)
            );
            let default_settings = SystemSettings::default();
            if let Err(e) =
                map_storage.store_item(&mut buffer, &KEY_SYSTEM, &default_settings).await
            {
                error!("[STORAGE] Failed to heal settings: {:?}", defmt::Debug2Format(&e));
            }
            default_settings
        }
    };

    CACHED_SETTINGS.lock(|cell| {
        *cell.borrow_mut() = loaded;
    });

    loaded
}

/// Returns the current system settings from in-memory cache synchronously.
#[must_use]
pub fn get_settings() -> SystemSettings {
    CACHED_SETTINGS.lock(|cell| *cell.borrow())
}

/// Persists updated system settings to flash if changed, and updates the in-memory cache.
pub async fn save_settings(settings: SystemSettings) -> Result<(), &'static str> {
    let clamped = SystemSettings {
        brightness_percent: theme::clamp_brightness(settings.brightness_percent as i32) as u8,
    };

    // Skip flash write if settings haven't changed
    let current = get_settings();
    if current == clamped {
        return Ok(());
    }

    let _guard = FLASH_MUTEX.lock().await;
    let flash = BlockingAsync(FlashStorage::new());

    let mut map_storage = MapStorage::new(
        flash,
        const { MapConfig::new(SETTINGS_RANGE) },
        Cache::new(
            ArrayPageStates::<4>::new(),
            ArrayPagePointers::<4>::new(),
            ArrayKeyPointers::<AppKey, 4>::new(),
        ),
    );

    let mut buffer = [0u8; 512];
    match map_storage.store_item(&mut buffer, &KEY_SYSTEM, &clamped).await {
        Ok(()) => {
            info!(
                "[STORAGE] Successfully saved system settings (brightness={}%)",
                clamped.brightness_percent
            );
            CACHED_SETTINGS.lock(|cell| {
                *cell.borrow_mut() = clamped;
            });
            Ok(())
        }
        Err(e) => {
            error!("[STORAGE] Failed to save system settings: {:?}", defmt::Debug2Format(&e));
            Err("flash write failed")
        }
    }
}
