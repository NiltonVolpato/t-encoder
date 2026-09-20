// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Slint Platform implementation and event loop for LilyGO T-Encoder Pro.

use alloc::rc::Rc;
use core::cell::RefCell;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use esp_hal::delay::Delay;
use esp_hal::time::Instant;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, RepaintBufferType,
};
use slint::platform::{Key, PointerEventButton, WindowAdapter, WindowEvent};
use slint::PhysicalSize;

use super::buzzer::{Feedback, signal_feedback};
use super::display::{BUFFER_HEIGHT, Co5300, NativeRgb565, RENDER_HEIGHT, RENDER_STRIDE, RENDER_WIDTH};
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
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 0.5 });
        window.set_size(PhysicalSize::new(
            RENDER_WIDTH as u32,
            RENDER_HEIGHT as u32,
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
            let position = slint::LogicalPosition::new(point.x as f32, point.y as f32);
            match point.event {
                TouchEvent::Down => {
                    let _ = window.dispatch_event_with_result(WindowEvent::PointerPressed {
                        position,
                        button: PointerEventButton::Left,
                    });
                }
                TouchEvent::Move => {
                    let _ =
                        window.dispatch_event_with_result(WindowEvent::PointerMoved { position });
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

/// Waits for at most `timeout` for an input event, or indefinitely if `timeout` is `None`.
async fn next_input_event(timeout: Option<Duration>) -> Option<InputEvent> {
    let Some(timeout) = timeout else {
        return Some(INPUT_EVENTS.receive().await);
    };
    match select(INPUT_EVENTS.receive(), Timer::after(timeout)).await {
        Either::First(event) => Some(event),
        Either::Second(()) => None,
    }
}

#[inline(always)]
fn cycle_count() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        xtensa_lx::timer::get_cycle_count()
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        0
    }
}

/// Runs the main Slint event loop: awaits interrupt-driven inputs, advances animations, and renders updates.
pub async fn run_event_loop(window_holder: WindowHolder, mut display: Co5300) -> ! {
    let delay = Delay::new();

    // Framebuffer in fast internal SRAM (200 * 196 * 2 bytes = ~78.4 KiB)
    #[repr(align(16))]
    struct FrameBuffer([NativeRgb565; RENDER_STRIDE * BUFFER_HEIGHT]);

    static FRAME_BUFFER: static_cell::ConstStaticCell<FrameBuffer> =
        static_cell::ConstStaticCell::new(FrameBuffer(
            [NativeRgb565::new(0); RENDER_STRIDE * BUFFER_HEIGHT],
        ));

    let frame_buffer = &mut FRAME_BUFFER.take().0;

    let mut pending_event: Option<InputEvent> = None;
    let mut power_manager = app_shell::ScreenPowerManager::new();
    let mut perf_tracker = app_shell::PerfTracker::new();
    let mut last_activity = Instant::now();
    let loop_start_time = Instant::now();

    defmt::info!("Entering interrupt-driven Slint MCU event loop with power management");

    loop {
        super::profiler::poll();

        // 1. Advance Slint animations and timers first thing in the loop
        slint::platform::update_timers_and_animations();

        let Some(window) = window_holder.borrow().clone() else {
            Timer::after(Duration::from_millis(10)).await;
            continue;
        };

        // 2. Update screen power state machine based on idle duration
        let idle_micros = (Instant::now() - last_activity).as_micros();
        let idle_duration = core::time::Duration::from_micros(idle_micros);
        match power_manager.update(idle_duration) {
            app_shell::PowerTransition::DimTo(level) => {
                defmt::info!("Screen idle: dimming to brightness {}", level);
                let _ = display.set_brightness(level);
            }
            app_shell::PowerTransition::Sleep => {
                defmt::info!("Screen idle: entering screen sleep");
                let _ = display.display_off();
            }
            _ => {}
        }

        // 3. Process ALL pending input events before drawing
        let mut event = pending_event
            .take()
            .or_else(|| INPUT_EVENTS.try_receive().ok());
        while let Some(current_event) = event {
            super::profiler::on_input_event();
            let (wake_action, transition) = power_manager.handle_input();
            match transition {
                app_shell::PowerTransition::WakeFromSleep(level) => {
                    defmt::info!("Waking from sleep to brightness {}", level);
                    let _ = display.display_on();
                    let _ = display.set_brightness(level);
                    window.request_redraw();
                }
                app_shell::PowerTransition::WakeFromDim(level) => {
                    defmt::info!("Restoring full brightness from dim: {}", level);
                    let _ = display.set_brightness(level);
                }
                _ => {}
            }
            last_activity = Instant::now();

            if wake_action == app_shell::WakeAction::DispatchEvent {
                dispatch_input_event(&window, current_event);
            } else {
                defmt::info!("Wake touch swallowed while sleeping");
            }

            event = INPUT_EVENTS.try_receive().ok();
        }

        // 4. Render dirty regions (skip DMA transfers if screen is sleeping)
        if !power_manager.is_sleeping() {
            window.draw_if_needed(|renderer| {
                let r_start = cycle_count();
                super::profiler::start_render();
                let region = renderer.render(frame_buffer, RENDER_STRIDE);
                super::profiler::stop_render();
                let render_cycles = cycle_count().wrapping_sub(r_start);

                let t_start = cycle_count();
                let mut total_pixels = 0u32;
                let mut rect_count = 0u16;
                let mut first = true;
                for (origin, size) in region.iter_box() {
                    let raw_x = origin.x.max(0) as u16;
                    let raw_y = origin.y.max(0) as u16;

                    // Inflate dirty rect by 1 logical pixel to refresh bilinear-interpolated
                    // boundary subpixels on the display and eliminate ghost trails.
                    let x = raw_x.saturating_sub(1);
                    let y = raw_y.saturating_sub(1);
                    let right = (raw_x + size.width as u16 + 1).min(RENDER_WIDTH);
                    let bottom = (raw_y + size.height as u16 + 1).min(RENDER_HEIGHT);

                    let width = right.saturating_sub(x);
                    let height = bottom.saturating_sub(y);

                    total_pixels += (width as u32 * 2) * (height as u32 * 2);
                    rect_count += 1;
                    if !first {
                        delay.delay_micros(10); // Delay needed to avoid glitching.
                    }
                    let _ = display.write_region(
                        frame_buffer,
                        RENDER_STRIDE,
                        x,
                        y,
                        width,
                        height,
                    );
                    first = false;
                }
                let transfer_cycles = cycle_count().wrapping_sub(t_start);

                perf_tracker.record_frame(app_shell::FrameCycles {
                    render_cycles,
                    transfer_cycles,
                    dirty_pixels: total_pixels,
                    rect_count,
                });
            });
        }

        // 5. Emit periodic performance summary if window elapsed
        let now_since_start = core::time::Duration::from_micros(
            (Instant::now() - loop_start_time).as_micros() as u64,
        );
        if let Some(summary) = perf_tracker.take_summary(now_since_start) {
            defmt::info!(
                "[PERF] {=f32} FPS | render: avg {=f32}ms (max {=f32}ms) | transfer: avg {=f32}ms (max {=f32}ms) | dirty: {=f32}% ({} rects, {} frames)",
                summary.fps,
                summary.avg_render_ms,
                summary.max_render_ms,
                summary.avg_transfer_ms,
                summary.max_transfer_ms,
                summary.avg_dirty_percent,
                summary.total_rects,
                summary.frame_count,
            );
        }

        // 6. Check if animations are actively running right after drawing
        let animating = !power_manager.is_sleeping() && window.has_active_animations();

        // 6. Determine sleep timeout
        let timeout = if animating {
            window.request_redraw();
            Some(Duration::from_hz(60))
        } else {
            let current_idle =
                core::time::Duration::from_micros((Instant::now() - last_activity).as_micros());
            let power_timeout = power_manager
                .time_until_next_transition(current_idle)
                .map(|d| Duration::from_micros(d.as_micros() as u64));

            let slint_timeout = slint::platform::duration_until_next_timer_update()
                .map(|d| Duration::from_micros(d.as_micros() as u64));

            match (slint_timeout, power_timeout) {
                (Some(s), Some(p)) => Some(s.min(p)),
                (Some(s), None) => Some(s),
                (None, Some(p)) => Some(p),
                (None, None) => None,
            }
        };

        // 7. Await next event or animation/timer tick
        if let Some(event) = next_input_event(timeout).await {
            pending_event = Some(event);
        }
    }
}
