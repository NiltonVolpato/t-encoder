// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Battery voltage monitoring via ADC1_CH0 on GPIO1.
//!
//! The Waveshare ESP32-S3-Knob-Touch-LCD-1.8 features a 1:1 voltage divider
//! (R62 = 10k, R63 = 10k) connected to GPIO1 (ADC1 Channel 0).
//! V_bat = 2 * V_adc.
//! Fully charged LiPo is ~4.2V (2.1V at ADC pin).
//! Depleted LiPo is ~3.3V (1.65V at ADC pin).
//! When USB is connected, VBUS (5.0V - 5.2V) powers the rail (bat_mv > 4350mV).

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use defmt::{info, trace, warn};
use embassy_time::Timer;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::peripherals::{ADC1, GPIO1};

static BATTERY_PERCENT: AtomicI32 = AtomicI32::new(100);
static IS_PLUGGED_IN: AtomicBool = AtomicBool::new(false);

/// Returns the latest cached battery percentage.
#[must_use]
pub fn get_battery_percent() -> i32 {
    BATTERY_PERCENT.load(Ordering::Relaxed)
}

/// Returns true if the device is currently plugged into USB power.
#[must_use]
pub fn is_plugged_in() -> bool {
    IS_PLUGGED_IN.load(Ordering::Relaxed)
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

    let mut last_logged_pct: i32 = -1;
    let mut last_logged_plugged: Option<bool> = None;
    let mut blink_toggle = false;
    let mut low_batt_haptic_counter: u32 = 0;

    loop {
        // Read raw voltage from ADC
        match adc.read_oneshot(&mut adc_pin) {
            Ok(raw_val) => {
                // With 11dB attenuation, full scale 4095 corresponds to ~2600mV at the pin.
                // V_pin_mv = (raw_val * 2600) / 4095
                // V_bat_mv = 2 * V_pin_mv = (raw_val * 5200) / 4095
                let bat_mv = (raw_val as u32 * 5200) / 4095;
                let plugged = bat_mv > 4350;
                let pct = if plugged { 100 } else { millivolts_to_percent(bat_mv) };

                BATTERY_PERCENT.store(pct, Ordering::Relaxed);
                IS_PLUGGED_IN.store(plugged, Ordering::Relaxed);

                // Log at info! only every 5% change, <=10% low battery, or plugged state transition
                let should_log_info = last_logged_plugged != Some(plugged)
                    || last_logged_pct < 0
                    || (pct != last_logged_pct && (pct % 5 == 0 || pct <= 10));

                if should_log_info {
                    info!("[BATTERY] bat_mv={}mV, percent={}%, plugged={}", bat_mv, pct, plugged);
                    last_logged_pct = pct;
                    last_logged_plugged = Some(plugged);
                } else {
                    trace!("[BATTERY] ADC raw={}, bat_mv={}mV, percent={}%", raw_val, bat_mv, pct);
                }

                // Low-battery handling (<= 10% on battery)
                if !plugged && pct <= 10 {
                    blink_toggle = !blink_toggle;
                    low_batt_haptic_counter += 1;
                    // Every 30 seconds (with 1-second task loop, 30 iterations)
                    if low_batt_haptic_counter >= 30 {
                        low_batt_haptic_counter = 0;
                        warn!("[BATTERY] Low battery warning ({}%), pulsing haptic alert", pct);
                        super::haptics::signal_feedback(super::haptics::Feedback::Haptic);
                    }
                } else {
                    blink_toggle = false;
                    low_batt_haptic_counter = 0;
                }

                theme::update_system_menu_state(|menu| {
                    menu.battery_percent = pct;
                    menu.is_plugged_in = plugged;
                    menu.low_battery_blink = blink_toggle;
                });

                Timer::after_secs(1).await;
            }
            Err(nb::Error::WouldBlock) => {
                // Expected while hardware SAR conversion completes (~10-20µs)
                Timer::after_micros(10).await;
            }
            Err(e) => {
                warn!("[BATTERY] ADC read error: {:?}", defmt::Debug2Format(&e));
                Timer::after_secs(1).await;
            }
        }
    }
}
