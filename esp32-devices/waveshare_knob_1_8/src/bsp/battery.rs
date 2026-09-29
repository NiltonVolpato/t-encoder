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

use core::cell::Cell;

use coprocessor::BatteryStatus;
use critical_section::Mutex;
use defmt::{info, trace, warn};
use embassy_time::Timer;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::peripherals::{ADC1, GPIO1};

/// Number of ADC readings averaged per measurement cycle.
const ADC_AVERAGING_SAMPLES: usize = 16;

static BATTERY_STATUS: Mutex<Cell<BatteryStatus>> =
    Mutex::new(Cell::new(BatteryStatus { millivolts: 0, percent: 0xFF, is_plugged: false }));

/// Returns the latest cached battery status.
#[must_use]
pub fn get_battery_status() -> BatteryStatus {
    critical_section::with(|cs| BATTERY_STATUS.borrow(cs).get())
}

/// Returns the latest cached battery percentage, or -1 if plugged/uninitialized.
#[must_use]
pub fn get_battery_percent() -> i32 {
    let s = get_battery_status();
    if s.percent == 0xFF { -1 } else { s.percent as i32 }
}

/// Returns the latest cached battery millivolts.
#[must_use]
pub fn get_battery_mv() -> u32 {
    get_battery_status().millivolts
}

/// Returns true if the device is currently plugged into USB power.
#[must_use]
pub fn is_plugged_in() -> bool {
    get_battery_status().is_plugged
}

/// Converts raw battery millivolts to percentage [0..=100] using KrX3D piecewise calibration curve.
#[inline]
pub fn millivolts_to_percent(mv: u32) -> i32 {
    const CURVE: &[(u32, i32)] = &[
        (3000, 0),
        (3300, 5),
        (3500, 10),
        (3600, 20),
        (3650, 30),
        (3700, 40),
        (3750, 50),
        (3800, 60),
        (3880, 70),
        (3950, 80),
        (4000, 90),
        (4030, 95),
        (4050, 100),
    ];

    if mv <= CURVE[0].0 {
        return 0;
    }
    if mv >= CURVE[CURVE.len() - 1].0 {
        return 100;
    }

    for i in 0..CURVE.len() - 1 {
        let (v0, p0) = CURVE[i];
        let (v1, p1) = CURVE[i + 1];
        if mv >= v0 && mv <= v1 {
            let span_v = (v1 - v0) as i32;
            let span_p = p1 - p0;
            return p0 + (((mv - v0) as i32 * span_p) / span_v);
        }
    }
    100
}

#[embassy_executor::task]
pub async fn battery_task(adc_peripheral: ADC1<'static>, pin: GPIO1<'static>) {
    info!("[BATTERY] Starting battery ADC task on GPIO1...");

    let mut adc_config = AdcConfig::new();
    let mut adc_pin = adc_config
        .enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(pin, Attenuation::_11dB);
    let mut adc = Adc::new(adc_peripheral, adc_config);

    let mut last_logged_pct: i32 = -1;
    let mut last_logged_plugged: Option<bool> = None;
    let mut low_batt_haptic_counter: u32 = 0;
    let mut ema_mv: Option<u32> = None;

    loop {
        let mut samples: heapless::Vec<u16, ADC_AVERAGING_SAMPLES> = heapless::Vec::new();

        // Average multiple oneshot readings for noise immunity
        let mut samples_failed: usize = 0;

        loop {
            match adc.read_oneshot(&mut adc_pin) {
                Ok(raw) => {
                    let _ = samples.push(raw);
                    if samples.len() == ADC_AVERAGING_SAMPLES {
                        break;
                    }
                    Timer::after_millis(1).await;
                }
                Err(nb::Error::WouldBlock) => {
                    Timer::after_micros(20).await;
                    samples_failed += 1;
                    if samples_failed >= 10 * ADC_AVERAGING_SAMPLES {
                        warn!(
                            "[BATTERY] {} samples after {} attempts",
                            samples.len(),
                            samples.len() + samples_failed
                        );
                        break;
                    }
                }
                Err(e) => {
                    warn!("[BATTERY] ADC read error: {:?}", defmt::Debug2Format(&e));
                    break;
                }
            }
        }

        if samples.is_empty() {
            Timer::after_secs(10).await;
            continue;
        }

        samples.sort();
        let median = samples[samples.len() / 2];
        let raw = samples.iter().map(|s| *s as u32).sum::<u32>() / samples.len() as u32;
        if (median as u32) / 100 != raw / 100 {
            info!("[BATTERY] median={}mV average={}mV", 2 * median, 2 * raw);
        }

        // Times 2 because of the voltage divider (10k / 10k).
        let measured_mv = 2 * (median as u32);
        let plugged = measured_mv > 4300;

        // Reset EMA filter immediately on plugged/unplugged transition to avoid slewing
        if last_logged_plugged.is_some() && last_logged_plugged != Some(plugged) {
            ema_mv = None;
        }

        // Exponential Moving Average filter (alpha = 0.25)
        let bat_mv = match ema_mv {
            Some(prev) => {
                let smoothed = (prev * 3 + measured_mv + 2) / 4;
                ema_mv = Some(smoothed);
                smoothed
            }
            None => {
                ema_mv = Some(measured_mv);
                measured_mv
            }
        };

        let pct: u8 = if plugged { 0xFF } else { millivolts_to_percent(bat_mv) as u8 };
        let status = BatteryStatus::new(bat_mv, pct, plugged);

        critical_section::with(|cs| BATTERY_STATUS.borrow(cs).set(status));

        // State changes when plugged status changes, or when non-plugged percent changes
        let state_changed =
            last_logged_plugged != Some(plugged) || (!plugged && last_logged_pct != pct as i32);

        if state_changed {
            if plugged {
                info!("[BATTERY] bat_mv={}mV (raw={}), charging (plugged=true)", bat_mv, raw);
            } else {
                info!(
                    "[BATTERY] bat_mv={}mV (raw={}), percent={}%, plugged=false",
                    bat_mv, raw, pct
                );
            }
            last_logged_pct = if plugged { -1 } else { pct as i32 };
            last_logged_plugged = Some(plugged);
            super::coprocessor::send_battery_status(status);
        } else {
            trace!("[BATTERY] ADC raw={}, bat_mv={}mV, percent={}%", raw, bat_mv, pct);
        }

        // Low-battery handling (<= 10% on battery)
        if !plugged && pct <= 10 {
            low_batt_haptic_counter += 1;
            // Every 30 seconds (with 10-second task loop, 3 iterations)
            if low_batt_haptic_counter >= 3 {
                low_batt_haptic_counter = 0;
                warn!("[BATTERY] Low battery warning ({}%), pulsing haptic alert", pct);
                super::haptics::signal_feedback(super::haptics::Feedback::Haptic);
            }
        } else {
            low_batt_haptic_counter = 0;
        }

        theme::update_system_menu_state(|menu| {
            menu.battery_percent = if plugged { -1 } else { pct as i32 };
            menu.is_plugged_in = plugged;
        });

        Timer::after_secs(10).await;
    }
}
