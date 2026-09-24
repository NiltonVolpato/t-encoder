// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! CHSC5816 capacitive touch controller on async I2C for LilyGO T-Encoder Pro.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::i2c::I2c as AsyncI2c;
use esp_hal::Async;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{GPIO8, GPIO9};

use super::input::{InputEvent, send_input_event};

const TOUCH_ADDRESS: u8 = 0x2E;
const REG_POINT: u32 = 0x2000_002C;
const REG_BOOT_STATE: u32 = 0x2000_0018;

/// Watchdog timeout during an active stroke before assuming finger lifted.
/// Controller reports at ~80-100 Hz (~10-12 ms) while contact is active.
const STROKE_TIMEOUT: Duration = Duration::from_millis(35);

/// Time window after rotary button release in which touches are suppressed as phantom touches.
const PHANTOM_GRACE_MS: u64 = 100;

pub use common::event::{TouchEvent, TouchPoint};

/// Notifies the touch driver of button state changes to suppress phantom touches.
pub fn set_button(down: bool) {
    let at_ms = Instant::now().as_millis();
    common::channels::set_button_state(down, at_ms);
}

struct PhantomTracker {
    button_down: bool,
    button_up_ms: Option<u64>,
}

impl PhantomTracker {
    const fn new() -> Self {
        Self { button_down: false, button_up_ms: None }
    }

    fn sync(&mut self) {
        while let Some((down, at_ms)) = common::channels::try_receive_button_state() {
            if self.button_down != down {
                self.button_down = down;
                if !down {
                    self.button_up_ms = Some(at_ms);
                }
            }
        }
    }

    fn is_phantom(&self, at_ms: u64) -> bool {
        self.button_down
            || self.button_up_ms.is_some_and(|up| at_ms.saturating_sub(up) < PHANTOM_GRACE_MS)
    }
}

pub struct Chsc5816 {
    i2c: I2c<'static, Async>,
    int: Input<'static>,
    rst: Output<'static>,
}

impl Chsc5816 {
    pub fn new(i2c: I2c<'static, Async>, int_pin: GPIO9<'static>, rst_pin: GPIO8<'static>) -> Self {
        let int = Input::new(int_pin, InputConfig::default().with_pull(Pull::Up));
        let rst = Output::new(rst_pin, Level::High, OutputConfig::default());
        Self { i2c, int, rst }
    }

    pub async fn init(&mut self) -> Result<(), esp_hal::i2c::master::Error> {
        self.pulse_reset().await;
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&REG_BOOT_STATE.to_be_bytes());
        buf[4..8].copy_from_slice(&0u32.to_be_bytes());
        let _ = AsyncI2c::write(&mut self.i2c, TOUCH_ADDRESS, &buf).await;
        self.pulse_reset().await;
        Timer::after(Duration::from_millis(100)).await;
        self.pulse_reset().await;
        Timer::after(Duration::from_millis(50)).await;
        Ok(())
    }

    async fn pulse_reset(&mut self) {
        self.rst.set_low();
        Timer::after(Duration::from_millis(5)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(10)).await;
    }

    pub async fn wait_for_interrupt(&mut self) {
        self.int.wait_for_falling_edge().await;
    }

    /// Reads the current touch point from the controller over async I2C.
    pub async fn read(&mut self) -> Result<Option<TouchPoint>, esp_hal::i2c::master::Error> {
        let mut buf = [0u8; 8];
        let addr_bytes = REG_POINT.to_be_bytes();
        AsyncI2c::write(&mut self.i2c, TOUCH_ADDRESS, &addr_bytes).await?;
        AsyncI2c::read(&mut self.i2c, TOUCH_ADDRESS, &mut buf).await?;

        let [_status, fingers, x_l8, y_l8, _z, hi, id_event, _p2] = buf;
        let event = match (id_event >> 4) & 0x0F {
            0 => TouchEvent::Down,
            8 => TouchEvent::Move,
            4 => TouchEvent::Up,
            _ => return Ok(None),
        };

        if fingers == 0 && event != TouchEvent::Up {
            return Ok(None);
        }

        let x = (u16::from(hi & 0x0F) << 8) | u16::from(x_l8);
        let y = (u16::from(hi >> 4) << 8) | u16::from(y_l8);

        Ok(Some(TouchPoint { x, y, event }))
    }
}

/// Asynchronous Embassy task listening for touch interrupts and dispatching events.
#[embassy_executor::task]
pub async fn touch_task(mut touch: Chsc5816) {
    if let Err(_e) = touch.init().await {
        defmt::error!("touch: failed to initialize CHSC5816");
        return;
    }
    defmt::info!("touch: CHSC5816 initialized and task started");

    let mut tracker = PhantomTracker::new();
    let mut stroke_active = false;
    let mut stroke_suppressed = false;
    let mut last_point: Option<(u16, u16)> = None;

    loop {
        tracker.sync();

        if stroke_active {
            // Active stroke: wait for next report edge with watchdog timeout
            match select(touch.wait_for_interrupt(), Timer::after(STROKE_TIMEOUT)).await {
                Either::First(()) => {}
                Either::Second(()) => {
                    // Watchdog fired: finger was lifted without an explicit Up event
                    tracker.sync();
                    if !stroke_suppressed {
                        if let Some((x, y)) = last_point.take() {
                            send_input_event(InputEvent::Touch(TouchPoint {
                                x,
                                y,
                                event: TouchEvent::Up,
                            }));
                        }
                    } else {
                        last_point = None;
                    }
                    stroke_active = false;
                    stroke_suppressed = false;
                    continue;
                }
            }
        } else {
            // Idle: sleep indefinitely on hardware falling edge with zero I2C traffic
            touch.wait_for_interrupt().await;
        }

        tracker.sync();
        let at_ms = Instant::now().as_millis();

        match touch.read().await {
            Ok(Some(point)) => match point.event {
                TouchEvent::Down => {
                    stroke_active = true;
                    stroke_suppressed = tracker.is_phantom(at_ms);
                    last_point = Some((point.x, point.y));
                    if !stroke_suppressed {
                        send_input_event(InputEvent::Touch(point));
                    }
                }
                TouchEvent::Move => {
                    last_point = Some((point.x, point.y));
                    if tracker.is_phantom(at_ms) {
                        stroke_suppressed = true;
                    }
                    if !stroke_suppressed {
                        if !stroke_active {
                            stroke_active = true;
                            send_input_event(InputEvent::Touch(TouchPoint {
                                x: point.x,
                                y: point.y,
                                event: TouchEvent::Down,
                            }));
                        } else {
                            send_input_event(InputEvent::Touch(point));
                        }
                    }
                }
                TouchEvent::Up => {
                    stroke_active = false;
                    if !stroke_suppressed {
                        send_input_event(InputEvent::Touch(point));
                    }
                    stroke_suppressed = false;
                    last_point = None;
                }
            },
            Ok(None) => {
                if stroke_active {
                    stroke_active = false;
                    if !stroke_suppressed && let Some((x, y)) = last_point.take() {
                        send_input_event(InputEvent::Touch(TouchPoint {
                            x,
                            y,
                            event: TouchEvent::Up,
                        }));
                    }
                    stroke_suppressed = false;
                }
            }
            Err(_e) => {
                defmt::error!("touch: I2C read error");
                if stroke_active {
                    stroke_active = false;
                    if !stroke_suppressed && let Some((x, y)) = last_point.take() {
                        send_input_event(InputEvent::Touch(TouchPoint {
                            x,
                            y,
                            event: TouchEvent::Up,
                        }));
                    }
                    stroke_suppressed = false;
                }
            }
        }
    }
}
