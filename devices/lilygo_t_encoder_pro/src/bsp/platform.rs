// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Slint Platform implementation and event loop for LilyGO T-Encoder Pro.

use alloc::rc::Rc;
use core::cell::RefCell;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use esp_hal::time::Instant;
use slint::PhysicalSize;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{
    Key, PointerEventButton, WindowAdapter, WindowEvent, WindowEventDispatchResult,
};

use super::buzzer;
use super::display::{
    BUFFER_HEIGHT, DISPLAY_COMMAND_CHANNEL, DirtyRect, DisplayCommand, FLUSH_RETURN_CHANNEL,
    FlushJob, Framebuffer, NativeRgb565, RENDER_HEIGHT, RENDER_STRIDE, RENDER_WIDTH,
    ReturnedBuffer,
};
use super::event::{EVENTS, Event, ScreenEvent};
use super::input::InputEvent;
use super::touch::TouchEvent;
use crate::tasks::report_user_activity;

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
        let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 0.5 });
        window.set_size(PhysicalSize::new(RENDER_WIDTH as u32, RENDER_HEIGHT as u32));
        self.window.replace(Some(window.clone()));
        Ok(window)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros((Instant::now() - self.start_time).as_micros())
    }
}

/// Dispatches to Slint and logs the actual result, instead of silently
/// discarding it like `let _ = window.dispatch_event_with_result(...)` did.
fn dispatch(window: &Rc<MinimalSoftwareWindow>, event: WindowEvent) {
    match window.dispatch_event_with_result(event.clone()) {
        Ok(WindowEventDispatchResult::Accepted) => {
            defmt::trace!("Slint accepted {:?} event", defmt::Debug2Format(&event));
        }
        Ok(WindowEventDispatchResult::Rejected) => {
            defmt::trace!("Slint rejected {:?} event", defmt::Debug2Format(&event));
        }
        Ok(_) => {}
        Err(_) => {
            defmt::error!(
                "Slint dispatch_event_with_result errored on {:?} event",
                defmt::Debug2Format(&event)
            );
        }
    }
}

/// Dispatches a single input event to the active Slint window.
fn dispatch_input_event(window: &Rc<MinimalSoftwareWindow>, event: InputEvent) {
    match event {
        InputEvent::Rotate(delta) => {
            if delta > 0 {
                buzzer::signal_feedback(buzzer::Feedback::DialStepForward);
                for _ in 0..delta {
                    dispatch(
                        window,
                        WindowEvent::KeyPressed {
                            text: Key::UpArrow.into(),
                        },
                    );
                    dispatch(
                        window,
                        WindowEvent::KeyReleased {
                            text: Key::UpArrow.into(),
                        },
                    );
                }
            } else if delta < 0 {
                buzzer::signal_feedback(buzzer::Feedback::DialStepBackward);
                for _ in 0..(-delta) {
                    dispatch(
                        window,
                        WindowEvent::KeyPressed {
                            text: Key::DownArrow.into(),
                        },
                    );
                    dispatch(
                        window,
                        WindowEvent::KeyReleased {
                            text: Key::DownArrow.into(),
                        },
                    );
                }
            }
        }
        InputEvent::Click => {
            buzzer::signal_feedback(buzzer::Feedback::Click);
            dispatch(
                window,
                WindowEvent::KeyPressed {
                    text: Key::Return.into(),
                },
            );
            dispatch(
                window,
                WindowEvent::KeyReleased {
                    text: Key::Return.into(),
                },
            );
        }
        InputEvent::LongPress => {
            buzzer::signal_feedback(buzzer::Feedback::Haptic);
            dispatch(
                window,
                WindowEvent::KeyPressed {
                    text: Key::Escape.into(),
                },
            );
            dispatch(
                window,
                WindowEvent::KeyReleased {
                    text: Key::Escape.into(),
                },
            );
        }
        InputEvent::Touch(point) => {
            let position = slint::LogicalPosition::new(point.x as f32, point.y as f32);
            match point.event {
                TouchEvent::Down => {
                    dispatch(
                        window,
                        WindowEvent::PointerPressed {
                            position,
                            button: PointerEventButton::Left,
                        },
                    );
                }
                TouchEvent::Move => {
                    dispatch(window, WindowEvent::PointerMoved { position });
                }
                TouchEvent::Up => {
                    dispatch(
                        window,
                        WindowEvent::PointerReleased {
                            position,
                            button: PointerEventButton::Left,
                        },
                    );
                    dispatch(window, WindowEvent::PointerExited);
                }
            }
        }
    }
}

/// Waits for at most `timeout` for a system event, or indefinitely if `timeout` is `None`.
async fn next_event(timeout: Option<Duration>) -> Option<Event> {
    let Some(timeout) = timeout else {
        return Some(EVENTS.receive().await);
    };
    match select(EVENTS.receive(), Timer::after(timeout)).await {
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
pub async fn run_event_loop(window_holder: WindowHolder) -> ! {
    // Two framebuffers in fast internal SRAM (2 * ~78.4 KiB = ~156.8 KiB)
    #[repr(align(16))]
    struct FrameBuffer([NativeRgb565; RENDER_STRIDE * BUFFER_HEIGHT]);

    static FRAME_BUFFER_A: static_cell::ConstStaticCell<FrameBuffer> =
        static_cell::ConstStaticCell::new(FrameBuffer(
            [NativeRgb565::new(0); RENDER_STRIDE * BUFFER_HEIGHT],
        ));
    static FRAME_BUFFER_B: static_cell::ConstStaticCell<FrameBuffer> =
        static_cell::ConstStaticCell::new(FrameBuffer(
            [NativeRgb565::new(0); RENDER_STRIDE * BUFFER_HEIGHT],
        ));

    let fb_a = Framebuffer(&mut FRAME_BUFFER_A.take().0);
    let fb_b = Framebuffer(&mut FRAME_BUFFER_B.take().0);
    if FLUSH_RETURN_CHANNEL
        .try_send(ReturnedBuffer {
            fb: fb_a,
            render_cycles: 0,
            transfer_cycles: 0,
            dirty_pixels: 0,
            rect_count: 0,
        })
        .is_err()
    {
        defmt::error!("FLUSH_RETURN_CHANNEL full while seeding framebuffer A, buffer lost");
    }
    if FLUSH_RETURN_CHANNEL
        .try_send(ReturnedBuffer {
            fb: fb_b,
            render_cycles: 0,
            transfer_cycles: 0,
            dirty_pixels: 0,
            rect_count: 0,
        })
        .is_err()
    {
        defmt::error!("FLUSH_RETURN_CHANNEL full while seeding framebuffer B, buffer lost");
    }

    let mut pending_event: Option<Event> = None;
    let mut perf_tracker = app_shell::PerfTracker::new();
    let loop_start_time = Instant::now();

    let base_brightness: u8 = 255;
    let mut is_dimmed: bool = false;
    let mut is_sleeping: bool = false;

    defmt::info!("Entering unified event-driven Slint MCU event loop");

    loop {
        // 1. Advance Slint animations and timers first thing in the loop
        slint::platform::update_timers_and_animations();

        let Some(window) = window_holder.borrow().clone() else {
            Timer::after(Duration::from_millis(10)).await;
            continue;
        };

        // 2. Process ALL pending events before drawing
        let event = pending_event.take().or_else(|| EVENTS.try_receive().ok());
        if let Some(current_event) = event {
            match current_event {
                Event::Input(input) => {
                    report_user_activity();

                    if is_sleeping {
                        defmt::info!("Waking display from sleep");
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::DisplayOn)
                            .await;
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::SetBrightness(base_brightness))
                            .await;
                        is_sleeping = false;
                        is_dimmed = false;
                        window.request_redraw();
                    } else {
                        if is_dimmed {
                            defmt::info!("Restoring full brightness from dim");
                            DISPLAY_COMMAND_CHANNEL
                                .send(DisplayCommand::SetBrightness(base_brightness))
                                .await;
                            is_dimmed = false;
                        }
                        dispatch_input_event(&window, input);
                    }
                }
                Event::Screen(screen_event) => match screen_event {
                    ScreenEvent::DimRelative(ratio) => {
                        let level =
                            ((base_brightness as f32 * ratio + 0.5) as u32).clamp(1, 255) as u8;
                        defmt::info!("Screen dimming to relative brightness {}", level);
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::SetBrightness(level))
                            .await;
                        is_dimmed = true;
                    }
                    ScreenEvent::DimAbsolute(level) => {
                        defmt::info!("Screen dimming to absolute brightness {}", level);
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::SetBrightness(level))
                            .await;
                        is_dimmed = true;
                    }
                    ScreenEvent::TurnOff => {
                        defmt::info!("Turning off display panel");
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::DisplayOff)
                            .await;
                        is_sleeping = true;
                    }
                    ScreenEvent::TurnOn => {
                        defmt::info!("Turning on display panel");
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::DisplayOn)
                            .await;
                        DISPLAY_COMMAND_CHANNEL
                            .send(DisplayCommand::SetBrightness(base_brightness))
                            .await;
                        is_sleeping = false;
                        is_dimmed = false;
                        window.request_redraw();
                    }
                },
            }
        }

        // 3. Render dirty regions (skip DMA transfers if screen is sleeping)
        let mut drew_frame = false;
        if !is_sleeping {
            let mut dirty_rects: heapless::Vec<DirtyRect, 3> = heapless::Vec::new();
            let mut total_pixels = 0u32;
            let mut rect_count = 0u16;
            let mut render_cycles = 0u32;

            let mut framebuffer: Option<ReturnedBuffer> = None;
            window.draw_if_needed(|renderer| {
                let Some(fb) = FLUSH_RETURN_CHANNEL.try_receive().ok() else {
                    defmt::warn!("skipping frame: no return buffer available");
                    return;
                };

                if fb.transfer_cycles > 0 {
                    perf_tracker.record_frame(app_shell::FrameCycles {
                        render_cycles: fb.render_cycles,
                        transfer_cycles: fb.transfer_cycles,
                        dirty_pixels: fb.dirty_pixels,
                        rect_count: fb.rect_count,
                    });
                }

                let r_start = cycle_count();
                let region = renderer.render(&mut fb.fb.0[..], RENDER_STRIDE);
                render_cycles = cycle_count().wrapping_sub(r_start);
                framebuffer = Some(fb);

                for (origin, size) in region.iter_box() {
                    let raw_x = origin.x.max(0) as u16;
                    let raw_y = origin.y.max(0) as u16;
                    let width = size.width as u16;
                    let height = size.height as u16;

                    total_pixels += (width as u32 * 2) * (height as u32 * 2);
                    rect_count += 1;

                    let _ = dirty_rects.push(DirtyRect {
                        x: raw_x,
                        y: raw_y,
                        width,
                        height,
                    });
                }
            });

            if let Some(fb) = framebuffer {
                drew_frame = !dirty_rects.is_empty();
                DISPLAY_COMMAND_CHANNEL
                    .send(DisplayCommand::Flush(FlushJob {
                        fb: fb.fb,
                        rects: dirty_rects,
                        render_cycles,
                        total_pixels,
                        rect_count,
                    }))
                    .await;
            }
        }

        // 4. Emit periodic performance summary if window elapsed
        let now_since_start =
            core::time::Duration::from_micros((Instant::now() - loop_start_time).as_micros());
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

        // 5. Determine timeout for event wait
        let animating = !is_sleeping && (window.has_active_animations() || drew_frame);
        let timeout = if animating {
            // window.request_redraw();
            Some(Duration::from_hz(60))
        } else {
            slint::platform::duration_until_next_timer_update()
                .map(|d| Duration::from_micros(d.as_micros() as u64))
        };

        // 6. Await next event or animation/timer tick
        if let Some(event) = next_event(timeout).await {
            pending_event = Some(event);
        }
    }
}
