//! Flash key-value settings storage backed by `esp-storage` and `sequential-storage`.
//!
//! Stores user configuration (such as Macropad rotary & press key bindings)
//! in the dedicated 16 KiB `settings` partition (`0xFFC000`..`0x1000000`).

pub use apps::{MacroBinding, MacropadSettings, Profile};
use core::cell::RefCell;
use core::ops::Range;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
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

/// Application settings identifier key (up to 16 ASCII bytes, e.g. "macropad").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppKey(pub [u8; 16]);

impl AppKey {
    /// Creates an `AppKey` from a string slice, padding with zeros up to 16 bytes.
    #[must_use]
    #[expect(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
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
        let length = self
            .0
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(self.0.len());
        let slice = self.0.get(..length).unwrap_or(&self.0);
        core::str::from_utf8(slice).unwrap_or("<invalid-utf8>")
    }
}

impl core::fmt::Display for AppKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
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

/// Map key for Macropad key bindings.
pub const KEY_MACROPAD: AppKey = AppKey::from_str("macropad");

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(transparent)]
struct StoredMacropadSettings(pub MacropadSettings);

impl PostcardValue<'_> for StoredMacropadSettings {}

/// In-memory cached Macropad settings for instant read access across cores/tasks.
static CACHED_MACROPAD: BlockingMutex<CriticalSectionRawMutex, RefCell<Option<MacropadSettings>>> =
    BlockingMutex::new(RefCell::new(None));

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
            ArrayKeyPointers::<AppKey, 4>::new(),
        ),
    );

    let mut buffer = [0u8; 2048];
    let loaded: Option<MacropadSettings> = match map_storage
        .fetch_item::<StoredMacropadSettings>(&mut buffer, &KEY_MACROPAD)
        .await
    {
        Ok(Some(stored)) => {
            log::info!("storage: loaded {KEY_MACROPAD} settings from flash");
            Some(stored.0)
        }
        Ok(None) => {
            log::info!("storage: no {KEY_MACROPAD} settings found, writing defaults");
            let default_settings = MacropadSettings::default();
            if let Err(error) = map_storage
                .store_item(
                    &mut buffer,
                    &KEY_MACROPAD,
                    &StoredMacropadSettings(default_settings.clone()),
                )
                .await
            {
                log::error!("storage: failed to store default {KEY_MACROPAD} settings: {error:?}");
            }
            Some(default_settings)
        }
        Err(error) => {
            log::warn!(
                "storage: failed to fetch settings for '{KEY_MACROPAD}' ({error:?}), writing defaults"
            );
            let default_settings = MacropadSettings::default();
            if let Err(heal_error) = map_storage
                .store_item(
                    &mut buffer,
                    &KEY_MACROPAD,
                    &StoredMacropadSettings(default_settings.clone()),
                )
                .await
            {
                log::error!(
                    "storage: failed to heal {KEY_MACROPAD} settings in flash: {heal_error:?}"
                );
            }
            Some(default_settings)
        }
    };

    CACHED_MACROPAD.lock(|cell| {
        *cell.borrow_mut() = loaded;
    });
}

/// Returns the current Macropad settings synchronously from in-memory cache.
#[must_use]
pub fn get_macropad_settings_sync() -> MacropadSettings {
    CACHED_MACROPAD.lock(|cell| cell.borrow().clone().unwrap_or_default())
}

/// Returns the current Macropad settings from in-memory cache.
#[must_use]
pub fn get_macropad_settings() -> MacropadSettings {
    get_macropad_settings_sync()
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
            ArrayKeyPointers::<AppKey, 4>::new(),
        ),
    );

    let mut buffer = [0u8; 2048];
    match map_storage
        .store_item(
            &mut buffer,
            &KEY_MACROPAD,
            &StoredMacropadSettings(settings.clone()),
        )
        .await
    {
        Ok(()) => {
            log::info!("storage: successfully saved {KEY_MACROPAD} settings to flash");
            CACHED_MACROPAD.lock(|cell| {
                *cell.borrow_mut() = Some(settings);
            });
            Ok(())
        }
        Err(error) => {
            log::error!("storage: failed to save {KEY_MACROPAD} settings: {error:?}");
            Err("flash write failed")
        }
    }
}
