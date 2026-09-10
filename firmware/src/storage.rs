//! Flash key-value settings storage backed by `esp-storage` and `sequential-storage`.
//!
//! Stores user configuration (such as Macropad rotary & press key bindings)
//! in the dedicated 16 KiB `settings` partition (`0xFFC000`..`0x1000000`).

use core::ops::Range;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash as SyncMultiwriteNorFlash, NorFlash as SyncNorFlash,
    ReadNorFlash as SyncReadNorFlash,
};
use embedded_storage_async::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use sequential_storage::{
    cache::{
        Cache, key_pointers::ArrayKeyPointers, page_pointers::ArrayPagePointers,
        page_states::ArrayPageStates,
    },
    map::{MapConfig, MapStorage, PostcardValue},
};

/// Partition range for `settings` from `partitions.csv`: 0xFFC000 (16 KiB = 4 pages).
const SETTINGS_RANGE: Range<u32> = 0x00FF_C000..0x0100_0000;

/// Map key for Macropad key bindings.
const KEY_MACROPAD: u8 = 1;

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

/// Configurable key binding for rotary or button input.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MacroBinding {
    pub label: heapless::String<32>,
    pub modifiers: u8,
    pub usage: u8,
}

/// Settings for the Macropad application.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MacropadSettings {
    pub rotate_cw: MacroBinding,
    pub rotate_ccw: MacroBinding,
    pub press: MacroBinding,
}

impl PostcardValue<'_> for MacropadSettings {}

impl Default for MacropadSettings {
    fn default() -> Self {
        let mut clockwise_label = heapless::String::new();
        let _ = clockwise_label.push_str("Scroll Down");
        let mut counter_clockwise_label = heapless::String::new();
        let _ = counter_clockwise_label.push_str("Scroll Up");
        let mut press_label = heapless::String::new();
        let _ = press_label.push_str("Play/Pause");

        Self {
            rotate_cw: MacroBinding {
                label: clockwise_label,
                modifiers: 0,
                usage: 0x51, // DOWN ARROW
            },
            rotate_ccw: MacroBinding {
                label: counter_clockwise_label,
                modifiers: 0,
                usage: 0x52, // UP ARROW
            },
            press: MacroBinding {
                label: press_label,
                modifiers: 0,
                usage: 0x2C, // SPACE
            },
        }
    }
}

/// In-memory cached Macropad settings for instant read access across cores/tasks.
static CACHED_MACROPAD: Mutex<CriticalSectionRawMutex, Option<MacropadSettings>> = Mutex::new(None);

/// Hardware flash access mutex so concurrent operations don't collide.
static FLASH_MUTEX: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());

/// Initializes flash storage, loads persisted settings, or stores defaults if flash is empty.
pub async fn init() {
    let _guard = FLASH_MUTEX.lock().await;
    let flash = BlockingAsync(FlashStorage::new());

    let mut map_storage = MapStorage::new(
        flash,
        const { MapConfig::new(SETTINGS_RANGE) },
        Cache::new(
            ArrayPageStates::<4>::new(),
            ArrayPagePointers::<4>::new(),
            ArrayKeyPointers::<u8, 4>::new(),
        ),
    );

    let mut buffer = [0u8; 256];
    let loaded: Option<MacropadSettings> = match map_storage
        .fetch_item::<MacropadSettings>(&mut buffer, &KEY_MACROPAD)
        .await
    {
        Ok(Some(settings)) => {
            log::info!("storage: loaded macropad settings from flash");
            Some(settings)
        }
        Ok(None) => {
            log::info!("storage: no macropad settings found, writing defaults");
            let default_settings = MacropadSettings::default();
            if let Err(e) = map_storage
                .store_item(&mut buffer, &KEY_MACROPAD, &default_settings)
                .await
            {
                log::error!("storage: failed to store default settings: {e:?}");
            }
            Some(default_settings)
        }
        Err(e) => {
            log::warn!("storage: failed to fetch settings ({e:?}), using default");
            Some(MacropadSettings::default())
        }
    };

    let mut cache = CACHED_MACROPAD.lock().await;
    *cache = loaded;
}

/// Returns the current Macropad settings from in-memory cache.
pub async fn get_macropad_settings() -> MacropadSettings {
    let cache = CACHED_MACROPAD.lock().await;
    cache.clone().unwrap_or_default()
}

/// Persists updated Macropad settings to flash and updates in-memory cache.
///
/// # Errors
///
/// Returns an error if writing to flash fails.
pub async fn save_macropad_settings(settings: MacropadSettings) -> Result<(), &'static str> {
    let _guard = FLASH_MUTEX.lock().await;
    let flash = BlockingAsync(FlashStorage::new());

    let mut map_storage = MapStorage::new(
        flash,
        const { MapConfig::new(SETTINGS_RANGE) },
        Cache::new(
            ArrayPageStates::<4>::new(),
            ArrayPagePointers::<4>::new(),
            ArrayKeyPointers::<u8, 4>::new(),
        ),
    );

    let mut buffer = [0u8; 256];
    match map_storage
        .store_item(&mut buffer, &KEY_MACROPAD, &settings)
        .await
    {
        Ok(()) => {
            log::info!("storage: successfully saved macropad settings to flash");
            let mut cache = CACHED_MACROPAD.lock().await;
            *cache = Some(settings);
            Ok(())
        }
        Err(e) => {
            log::error!("storage: failed to save macropad settings: {e:?}");
            Err("flash write failed")
        }
    }
}
