// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Battery voltage monitoring via ADC1_CH0 on GPIO1.
//!
//! The Waveshare ESP32-S3-Knob-Touch-LCD-1.8 features a 1:1 voltage divider
//! (R62 = 10k, R63 = 10k) connected to GPIO1 (ADC1 Channel 0).
//! V_bat = 2 * V_adc.
//! Fully charged LiPo is ~4.2V (2.1V at ADC pin).
//! Depleted LiPo is ~3.3V (1.65V at ADC pin).

use core::sync::atomic::{AtomicI32, Ordering};

use defmt::{debug, info};
use embassy_time::Timer;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::peripherals::{ADC1, GPIO1};

static BATTERY_PERCENT: AtomicI32 = AtomicI32::new(100);

/// Returns the latest cached battery percentage.
#[must_use]
pub fn get_battery_percent() -> i32 {
    BATTERY_PERCENT.load(Ordering::Relaxed)
}

/// Converts raw battery millivolts to percentage [0..=100].
#[inline]
pub fn millivolts_to_percent(mv: u32) -> i32 {
    const MIN_MV: u32 = 3300;
    const MAX_MV: u32 = 4200;

    if mv <= MIN_MV {
        0
    } else if mv >= MAX_MV {
        100
    } else {
        (((mv - MIN_MV) * 100) / (MAX_MV - MIN_MV)) as i32
    }
}

#[embassy_executor::task]
pub async fn battery_task(adc_peripheral: ADC1<'static>, pin: GPIO1<'static>) {
    info!("[BATTERY] Starting battery ADC task on GPIO1...");

    let mut adc_config = AdcConfig::new();
    let mut adc_pin = adc_config.enable_pin(pin, Attenuation::_11dB);
    let mut adc = Adc::new(adc_peripheral, adc_config);

    loop {
        // Read raw voltage from ADC
        match adc.read_oneshot(&mut adc_pin) {
            Ok(raw_val) => {
                // With 11dB attenuation, full scale 4095 corresponds to ~2600mV at the pin.
                // V_pin_mv = (raw_val * 2600) / 4095
                // V_bat_mv = 2 * V_pin_mv = (raw_val * 5200) / 4095
                let bat_mv = (raw_val as u32 * 5200) / 4095;
                let pct = millivolts_to_percent(bat_mv);
                BATTERY_PERCENT.store(pct, Ordering::Relaxed);
                theme::update_system_menu_state(|menu| {
                    menu.battery_percent = pct;
                });
                debug!("[BATTERY] ADC raw={}, bat_mv={}mV, percent={}%", raw_val, bat_mv, pct);
                Timer::after_secs(5).await;
            }
            Err(nb::Error::WouldBlock) => {
                // Expected while hardware SAR conversion completes (~10-20µs)
                Timer::after_micros(10).await;
            }
            Err(e) => {
                defmt::warn!("[BATTERY] ADC read error: {:?}", defmt::Debug2Format(&e));
                Timer::after_secs(5).await;
            }
        }
    }
}
