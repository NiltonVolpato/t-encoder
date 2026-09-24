// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! CST816D capacitive touch screen task for Waveshare Knob 1.8.

use cst816::{Cst816, TouchState};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use esp_hal::Async;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{GPIO9, GPIO10};

use common::channels::send_input_event;
use common::event::{InputEvent, TouchEvent, TouchPoint};

/// Watchdog timeout during an active stroke before assuming finger lifted.
const STROKE_TIMEOUT: Duration = Duration::from_millis(40);

pub struct TouchHw {
    cst: Cst816<I2c<'static, Async>>,
    int: Input<'static>,
    rst: Output<'static>,
}

impl TouchHw {
    pub fn new(i2c: I2c<'static, Async>, int_pin: GPIO9<'static>, rst_pin: GPIO10<'static>) -> Self {
        let int = Input::new(int_pin, InputConfig::default().with_pull(Pull::Up));
        let rst = Output::new(rst_pin, Level::High, OutputConfig::default());
        let cst = Cst816::new(i2c);
        Self { cst, int, rst }
    }

    pub async fn reset(&mut self) {
        self.rst.set_low();
        Timer::after(Duration::from_millis(5)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(50)).await;
    }

    pub async fn wait_for_interrupt(&mut self) {
        self.int.wait_for_falling_edge().await;
    }

    pub async fn read(&mut self) -> Result<Option<TouchPoint>, esp_hal::i2c::master::Error> {
        match self.cst.read_touch().await {
            Ok(Some(pt)) => {
                let event = match pt.state {
                    TouchState::Down => TouchEvent::Down,
                    TouchState::Contact => TouchEvent::Move,
                    TouchState::Up => TouchEvent::Up,
                };
                Ok(Some(TouchPoint { x: pt.x, y: pt.y, event }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Asynchronous Embassy task listening for touch interrupts and dispatching events.
#[embassy_executor::task]
pub async fn touch_task(mut touch: TouchHw) {
    touch.reset().await;
    defmt::info!("touch: CST816D initialized and task started");

    let mut stroke_active = false;
    let mut last_point: Option<(u16, u16)> = None;

    loop {
        if stroke_active {
            match select(touch.wait_for_interrupt(), Timer::after(STROKE_TIMEOUT)).await {
                Either::First(()) => {}
                Either::Second(()) => {
                    // Watchdog fired: finger was lifted without an explicit Up event
                    if let Some((x, y)) = last_point.take() {
                        send_input_event(InputEvent::Touch(TouchPoint {
                            x,
                            y,
                            event: TouchEvent::Up,
                        }));
                    }
                    stroke_active = false;
                    continue;
                }
            }
        } else {
            touch.wait_for_interrupt().await;
        }

        match touch.read().await {
            Ok(Some(point)) => match point.event {
                TouchEvent::Down => {
                    stroke_active = true;
                    last_point = Some((point.x, point.y));
                    send_input_event(InputEvent::Touch(point));
                }
                TouchEvent::Move => {
                    last_point = Some((point.x, point.y));
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
                TouchEvent::Up => {
                    stroke_active = false;
                    send_input_event(InputEvent::Touch(point));
                    last_point = None;
                }
            },
            Ok(None) => {
                if stroke_active {
                    stroke_active = false;
                    if let Some((x, y)) = last_point.take() {
                        send_input_event(InputEvent::Touch(TouchPoint {
                            x,
                            y,
                            event: TouchEvent::Up,
                        }));
                    }
                }
            }
            Err(_e) => {
                defmt::error!("touch: I2C read error");
                if stroke_active {
                    stroke_active = false;
                    if let Some((x, y)) = last_point.take() {
                        send_input_event(InputEvent::Touch(TouchPoint {
                            x,
                            y,
                            event: TouchEvent::Up,
                        }));
                    }
                }
            }
        }
    }
}
