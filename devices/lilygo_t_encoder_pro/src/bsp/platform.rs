// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Slint Platform implementation and event loop for LilyGO T-Encoder Pro.

use alloc::rc::Rc;
use core::cell::RefCell;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use esp_hal::delay::Delay;
use esp_hal::time::Instant;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Key, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{PhysicalPosition, PhysicalSize};
use static_cell::StaticCell;

use super::buzzer::{Feedback, signal_feedback};
use super::display::{Co5300, DISPLAY_HEIGHT, DISPLAY_WIDTH, DMA_CHUNK_SIZE};
use super::input::{INPUT_EVENTS, InputEvent};
use super::touch::TouchEvent;

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

/// Dispatches a single input event to the active Slint window.
fn dispatch_input_event(window: &Rc<MinimalSoftwareWindow>, event: InputEvent) {
    match event {
        InputEvent::Rotate(delta) => {
            if delta > 0 {
                signal_feedback(Feedback::DialStepForward);
                for _ in 0..delta {
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                        text: Key::UpArrow.into(),
                    });
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                        text: Key::UpArrow.into(),
                    });
                }
            } else if delta < 0 {
                signal_feedback(Feedback::DialStepBackward);
                for _ in 0..(-delta) {
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                        text: Key::DownArrow.into(),
                    });
                    let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                        text: Key::DownArrow.into(),
                    });
                }
            }
        }
        InputEvent::Click => {
            signal_feedback(Feedback::Click);
            let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                text: Key::Return.into(),
            });
            let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                text: Key::Return.into(),
            });
        }
        InputEvent::LongPress => {
            signal_feedback(Feedback::Haptic);
            let _ = window.dispatch_event_with_result(WindowEvent::KeyPressed {
                text: Key::Escape.into(),
            });
            let _ = window.dispatch_event_with_result(WindowEvent::KeyReleased {
                text: Key::Escape.into(),
            });
        }
        InputEvent::Touch(point) => {
            let position = PhysicalPosition::new(point.x as i32, point.y as i32)
                .to_logical(window.scale_factor());
            match point.event {
                TouchEvent::Down => {
                    let _ = window.dispatch_event_with_result(WindowEvent::PointerPressed {
                        position,
                        button: PointerEventButton::Left,
                    });
                }
                TouchEvent::Move => {
                    let _ = window.dispatch_event_with_result(WindowEvent::PointerMoved {
                        position,
                    });
                }
                TouchEvent::Up => {
                    let _ = window.dispatch_event_with_result(WindowEvent::PointerReleased {
                        position,
                        button: PointerEventButton::Left,
                    });
                    let _ = window.dispatch_event_with_result(WindowEvent::PointerExited);
                }
            }
        }
    }
}

/// Runs the main Slint event loop: awaits interrupt-driven inputs, advances animations, and renders updates.
pub async fn run_event_loop(window_holder: WindowHolder, mut display: Co5300) -> ! {
    let delay = Delay::new();

    // Framebuffer in external PSRAM (390 * 390 * 2 bytes = ~297 KiB)
    let mut frame_buffer =
        alloc::vec![Rgb565Pixel(0); DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize];

    // Scratch buffer in fast internal DRAM for DMA transfers
    static PIXEL_SCRATCH: StaticCell<[u8; DMA_CHUNK_SIZE]> = StaticCell::new();
    let scratch = PIXEL_SCRATCH.init([0u8; DMA_CHUNK_SIZE]);

    let mut pending_event: Option<InputEvent> = None;

    defmt::info!("Entering interrupt-driven Slint MCU event loop");

    loop {
        // 1. Advance Slint animations and timers first thing in the loop
        slint::platform::update_timers_and_animations();

        let Some(window) = window_holder.borrow().clone() else {
            Timer::after(Duration::from_millis(10)).await;
            continue;
        };

        // 2. Handle at most ONE input event before drawing
        if let Some(event) = pending_event.take() {
            dispatch_input_event(&window, event);
        } else if let Ok(event) = INPUT_EVENTS.try_receive() {
            dispatch_input_event(&window, event);
        }

        // 3. Render dirty regions
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

        // 4. Check if animations are actively running right after drawing
        let animating = window.has_active_animations();

        // 5. Determine sleep timeout
        let timeout = if animating {
            window.request_redraw();
            Some(Duration::from_millis(1))
        } else if let Some(timer_duration) = slint::platform::duration_until_next_timer_update() {
            Some(Duration::from_micros(timer_duration.as_micros() as u64))
        } else {
            None // Zero polling: sleep indefinitely until next input event
        };

        // 6. Await next event or animation/timer tick
        match timeout {
            Some(duration) => {
                match select(INPUT_EVENTS.receive(), Timer::after(duration)).await {
                    Either::First(event) => {
                        pending_event = Some(event);
                    }
                    Either::Second(()) => {}
                }
            }
            None => {
                let event = INPUT_EVENTS.receive().await;
                pending_event = Some(event);
            }
        }
    }
}
