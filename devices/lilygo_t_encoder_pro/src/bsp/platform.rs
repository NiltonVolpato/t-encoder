// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Slint Platform implementation and event loop for LilyGO T-Encoder Pro.

use alloc::rc::Rc;
use core::cell::RefCell;
use esp_hal::delay::Delay;
use esp_hal::time::Instant;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Key, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalPosition, PhysicalSize};
use static_cell::StaticCell;

use super::display::{Co5300, DISPLAY_HEIGHT, DISPLAY_WIDTH, DMA_CHUNK_SIZE};
use super::rotary::{ButtonEvent, Rotary};
use super::touch::{Chsc5816, TouchEvent};

pub type WindowHolder = Rc<RefCell<Option<Rc<MinimalSoftwareWindow>>>>;

pub struct EspPlatform {
    window: WindowHolder,
    start_time: Instant,
}

impl EspPlatform {
    pub fn new() -> (Self, WindowHolder) {
        let window: WindowHolder = Rc::new(RefCell::new(None));
        (
            Self {
                window: window.clone(),
                start_time: Instant::now(),
            },
            window,
        )
    }
}

impl slint::platform::Platform for EspPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(PhysicalSize::new(
            DISPLAY_WIDTH as u32,
            DISPLAY_HEIGHT as u32,
        ));
        self.window.replace(Some(window.clone()));
        Ok(window)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros((Instant::now() - self.start_time).as_micros())
    }
}

/// Runs the main Slint event loop: polling inputs, updating timers, and flushing display updates.
pub fn run_event_loop(
    window_holder: WindowHolder,
    mut display: Co5300,
    mut touch: Option<Chsc5816>,
    mut rotary: Rotary,
) -> ! {
    let delay = Delay::new();

    // Framebuffer in external PSRAM (390 * 390 * 2 bytes = ~297 KiB)
    let mut frame_buffer =
        alloc::vec![Rgb565Pixel(0); DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize];

    // Scratch buffer in fast internal DRAM for DMA transfers
    static PIXEL_SCRATCH: StaticCell<[u8; DMA_CHUNK_SIZE]> = StaticCell::new();
    let scratch = PIXEL_SCRATCH.init([0u8; DMA_CHUNK_SIZE]);

    let mut last_touch: Option<LogicalPosition> = None;

    defmt::info!("Entering Slint MCU event loop");

    loop {
        // 1. Advance Slint animations and timers
        slint::platform::update_timers_and_animations();

        let Some(window) = window_holder.borrow().clone() else {
            delay.delay_millis(10);
            continue;
        };

        // 2. Poll touch controller
        if let Some(ref mut touch_dev) = touch {
            match touch_dev.read() {
                Ok(Some(point)) => {
                    let position = PhysicalPosition::new(point.x as i32, point.y as i32)
                        .to_logical(window.scale_factor());
                    let event = match point.event {
                        TouchEvent::Down => {
                            last_touch = Some(position);
                            Some(WindowEvent::PointerPressed {
                                position,
                                button: PointerEventButton::Left,
                            })
                        }
                        TouchEvent::Move => {
                            last_touch = Some(position);
                            Some(WindowEvent::PointerMoved { position })
                        }
                        TouchEvent::Up => {
                            last_touch = None;
                            let _ = window.dispatch_event_with_result(WindowEvent::PointerReleased {
                                position,
                                button: PointerEventButton::Left,
                            });
                            Some(WindowEvent::PointerExited)
                        }
                    };
                    if let Some(event) = event {
                        let _ = window.dispatch_event_with_result(event);
                    }
                }
                Ok(None) => {
                    if let Some(position) = last_touch.take() {
                        let _ = window.dispatch_event_with_result(WindowEvent::PointerReleased {
                            position,
                            button: PointerEventButton::Left,
                        });
                        let _ = window.dispatch_event_with_result(WindowEvent::PointerExited);
                    }
                }
                Err(_e) => {}
            }
        }

        // 3. Poll rotary encoder rotation
        let delta = rotary.poll_rotation();
        if delta > 0 {
            for _ in 0..delta {
                let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                    text: Key::UpArrow.into(),
                });
                let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                    text: Key::UpArrow.into(),
                });
            }
        } else if delta < 0 {
            for _ in 0..(-delta) {
                let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                    text: Key::DownArrow.into(),
                });
                let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                    text: Key::DownArrow.into(),
                });
            }
        }

        // 4. Poll rotary button
        if let Some(btn_event) = rotary.poll_button() {
            match btn_event {
                ButtonEvent::Click => {
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                        text: Key::Return.into(),
                    });
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                        text: Key::Return.into(),
                    });
                }
                ButtonEvent::LongPress => {
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                        text: Key::Escape.into(),
                    });
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                        text: Key::Escape.into(),
                    });
                }
            }
        }

        // 5. Draw dirty regions
        window.draw_if_needed(|renderer| {
            let region = renderer.render(&mut frame_buffer, DISPLAY_WIDTH as usize);
            let mut first = true;
            for (origin, size) in region.iter() {
                if !first {
                    delay.delay_micros(10); // Delay needed to avoid glitching.
                }
                let _ = display.write_region(
                    &frame_buffer,
                    DISPLAY_WIDTH as usize,
                    origin.x as u16,
                    origin.y as u16,
                    size.width as u16,
                    size.height as u16,
                    scratch,
                );
                first = false;
            }
        });

        // 6. Sleep briefly when idle to save CPU cycles
        if !window.has_active_animations() {
            delay.delay_millis(10);
        }
    }
}
