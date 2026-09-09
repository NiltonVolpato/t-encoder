//! Simon Says memory game.
//!
//! Classic 4-quadrant electronic memory game for the round 390×390 AMOLED.
//!
//! Tap the pads to repeat the sequence of lights and tones played by Simon.
//! Each round adds one more step to the sequence, and speed accelerates
//! across levels.
//!
//! Designed with pure `no_std` state, testable on host with deterministic PRNG.

use heapless::Vec;
use launcher::{App, AppFactory, Ctx, IconId, InputEvent, Manifest, Outcome, TouchAccess, ViewId};
use ui::{Shell, SimonState};

/// Screen center coordinate for 390×390 round panel.
pub const CENTER: i32 = 195;

/// Radius of the center status hub in pixels.
pub const HUB_RADIUS: i32 = 70;

/// Radius of the outer circular playfield in pixels.
pub const OUTER_RADIUS: i32 = 190;

/// Maximum sequence length before winning (a perfect 1978 game is 31 signals).
pub const WIN_SEQUENCE: usize = 31;

/// Maximum sequence storage capacity.
pub const MAX_SEQUENCE: usize = 32;

/// Gap between tones during demonstration in milliseconds.
pub const GAP_DURATION_MS: u32 = 50;

/// Game over / timeout buzzer frequency (Razz) in Hertz.
pub const RAZZ_HZ: u32 = 42;

/// Game over / timeout buzzer duration in milliseconds.
pub const RAZZ_MS: u32 = 1000;

/// Four classic Simon colors and frequencies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimonColor {
    /// Top-Left, 220.00 Hz (A3).
    Yellow,
    /// Top-Right, 164.81 Hz (E3).
    Blue,
    /// Bottom-Left, 277.18 Hz (C#4).
    Red,
    /// Bottom-Right, 329.63 Hz (E4).
    Green,
}

impl SimonColor {
    /// Audio frequency in Hertz.
    #[must_use]
    pub const fn frequency_hz(self) -> u32 {
        match self {
            SimonColor::Blue => 165,
            SimonColor::Yellow => 220,
            SimonColor::Red => 277,
            SimonColor::Green => 330,
        }
    }
}

/// Hit target computed from panel coordinates (x, y).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    None,
    CenterHub,
    Pad(SimonColor),
}

/// Maps panel coordinates `(x, y)` to a [`HitTarget`].
#[must_use]
pub fn hit_test(x: i32, y: i32) -> HitTarget {
    let dx = x.saturating_sub(CENTER);
    let dy = y.saturating_sub(CENTER);
    let dist_sq = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));

    if dist_sq <= HUB_RADIUS * HUB_RADIUS {
        return HitTarget::CenterHub;
    }
    if dist_sq > OUTER_RADIUS * OUTER_RADIUS {
        return HitTarget::None;
    }

    match (dx < 0, dy < 0) {
        (true, true) => HitTarget::Pad(SimonColor::Yellow),
        (false, true) => HitTarget::Pad(SimonColor::Blue),
        (true, false) => HitTarget::Pad(SimonColor::Red),
        (false, false) => HitTarget::Pad(SimonColor::Green),
    }
}

/// Fast, compact Xorshift32 PRNG for sequence generation in `no_std`.
#[derive(Clone, Copy, Debug)]
pub struct Prng {
    state: u32,
}

impl Prng {
    /// Creates a PRNG with a non-zero initial seed.
    #[must_use]
    pub const fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x6d2b_79f5 } else { seed },
        }
    }

    /// Generates the next 32-bit pseudo-random unsigned integer.
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Generates a pseudo-random [`SimonColor`].
    pub fn next_color(&mut self) -> SimonColor {
        match self.next_u32() % 4 {
            0 => SimonColor::Yellow,
            1 => SimonColor::Blue,
            2 => SimonColor::Red,
            _ => SimonColor::Green,
        }
    }
}

/// Lifecycle phases of Simon Says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Title/attract screen before game starts.
    Idle,
    /// Simon is demonstrating the current sequence to the player.
    Demonstrating {
        step: usize,
        is_lit: bool,
        step_until_ms: u64,
    },
    /// Player's turn to repeat the demonstrated sequence.
    PlayerTurn {
        step: usize,
        timeout_at_ms: u64,
        feedback_until_ms: Option<u64>,
    },
    /// Brief pause after player successfully completes a round.
    RoundSuccess { until_ms: u64, pad_off_at_ms: u64 },
    /// Player completed all 31 steps (perfect game).
    Victory { until_ms: u64 },
    /// Player made a mistake or timed out; game over.
    GameOver { until_ms: u64 },
}

/// Builds [`Simon`] instances.
pub struct SimonFactory {
    manifest: Manifest,
    shell: slint::Weak<Shell>,
}

impl SimonFactory {
    /// Creates the factory against the shared Slint tree.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> SimonFactory {
        SimonFactory {
            manifest: Manifest {
                name: "Simon",
                icon: IconId(2),
                view: ViewId(3),
                // Golden yellow accent matching classic Simon branding.
                accent: embedded_graphics::pixelcolor::Rgb565::new(31, 55, 0),
                touch: TouchAccess::Taps,
            },
            shell,
        }
    }
}

impl AppFactory for SimonFactory {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> alloc::boxed::Box<dyn App + '_> {
        alloc::boxed::Box::new(Simon::new(self.shell.clone()))
    }
}

/// Running instance of Simon Says.
pub struct Simon {
    shell: slint::Weak<Shell>,
    prng: Prng,
    sequence: Vec<SimonColor, MAX_SEQUENCE>,
    phase: Phase,
    score: u32,
    high_score: u32,
    active_pad: Option<SimonColor>,
}

impl Simon {
    /// Creates a fresh, unset game instance in [`Phase::Idle`].
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> Self {
        Self {
            shell,
            prng: Prng::new(0),
            sequence: Vec::new(),
            phase: Phase::Idle,
            score: 0,
            high_score: 0,
            active_pad: None,
        }
    }

    /// Current game level based on sequence length milestone.
    ///
    /// Authentic 1978 Simon milestone thresholds:
    /// - Signals 1–5: Level 1 (relaxed tempo)
    /// - Signals 6–9: Level 2
    /// - Signals 10–13: Level 3
    /// - Signals 14–31: Level 4 (fastest tempo)
    #[must_use]
    pub fn level(&self) -> u32 {
        match self.sequence.len() {
            0..=5 => 1,
            6..=9 => 2,
            10..=13 => 3,
            _ => 4,
        }
    }

    /// Tone duration in milliseconds based on current sequence progression.
    ///
    /// Authentic 1978 Simon timing:
    /// - Signals 1–5: 420 ms
    /// - Signals 6–9: 320 ms
    /// - Signals 10–13: 220 ms
    /// - Signals 14–31: 140 ms
    #[must_use]
    pub const fn tone_duration_ms(len: usize) -> u32 {
        match len {
            0..=5 => 420,
            6..=9 => 320,
            10..=13 => 220,
            _ => 140,
        }
    }

    /// Response timeout window in milliseconds before Simon plays the 42 Hz Razz.
    ///
    /// Authentic 1978 Simon timeout limits:
    /// - Signals 1–5: 5.0 seconds
    /// - Signals 6–13: 3.0 seconds
    /// - Signals 14–31: 1.5 seconds
    #[must_use]
    pub const fn response_timeout_ms(len: usize) -> u64 {
        match len {
            0..=5 => 5_000,
            6..=13 => 3_000,
            _ => 1_500,
        }
    }

    /// Starts a new game from score 0.
    pub fn start_game(&mut self, now_ms: u64) -> Outcome {
        let max_seed = u64::from(u32::MAX);
        let rem = now_ms.checked_rem(max_seed).unwrap_or(0);
        let seed = u32::try_from(rem).unwrap_or(0);
        self.prng = Prng::new(seed);
        self.sequence.clear();
        self.score = 0;
        self.active_pad = None;

        // Add initial step
        let first_color = self.prng.next_color();
        let _ = self.sequence.push(first_color);

        let duration = Self::tone_duration_ms(self.sequence.len());
        self.phase = Phase::Demonstrating {
            step: 0,
            is_lit: true,
            step_until_ms: now_ms.saturating_add(u64::from(duration)),
        };
        self.active_pad = Some(first_color);

        Outcome::tone(first_color.frequency_hz(), duration)
    }

    /// Advances demonstration playback during [`Phase::Demonstrating`].
    fn tick_demonstration(
        &mut self,
        step: usize,
        is_lit: bool,
        step_until_ms: u64,
        now_ms: u64,
    ) -> Outcome {
        if now_ms < step_until_ms {
            return Outcome::NONE;
        }

        if is_lit {
            // Turn off current pad for 50 ms gap
            self.active_pad = None;
            self.phase = Phase::Demonstrating {
                step,
                is_lit: false,
                step_until_ms: now_ms.saturating_add(u64::from(GAP_DURATION_MS)),
            };
            Outcome::CHANGED
        } else {
            // Gap finished: next step or transition to player
            let next_step = step.saturating_add(1);
            if let Some(&color) = self.sequence.get(next_step) {
                let duration = Self::tone_duration_ms(self.sequence.len());
                self.active_pad = Some(color);
                self.phase = Phase::Demonstrating {
                    step: next_step,
                    is_lit: true,
                    step_until_ms: now_ms.saturating_add(u64::from(duration)),
                };
                Outcome::tone(color.frequency_hz(), duration)
            } else {
                // Done demonstrating sequence; hand over to player
                self.active_pad = None;
                let timeout = Self::response_timeout_ms(self.sequence.len());
                self.phase = Phase::PlayerTurn {
                    step: 0,
                    timeout_at_ms: now_ms.saturating_add(timeout),
                    feedback_until_ms: None,
                };
                Outcome::CHANGED
            }
        }
    }

    /// Handles player pad selection.
    fn handle_player_choice(&mut self, chosen: SimonColor, now_ms: u64) -> Outcome {
        let Phase::PlayerTurn { step, .. } = self.phase else {
            return Outcome::NONE;
        };

        let expected = self.sequence.get(step).copied();
        if Some(chosen) == expected {
            // Correct choice!
            self.active_pad = Some(chosen);
            let next_step = step.saturating_add(1);
            let duration = Self::tone_duration_ms(self.sequence.len());

            if next_step >= self.sequence.len() {
                // Completed the entire round sequence!
                self.score = self.score.saturating_add(1);
                if self.score > self.high_score {
                    self.high_score = self.score;
                }

                if self.sequence.len() >= WIN_SEQUENCE {
                    // Won the game! 31 signals completed!
                    self.phase = Phase::Victory {
                        until_ms: now_ms.saturating_add(3_000),
                    };
                    Outcome::tone(chosen.frequency_hz(), duration)
                } else {
                    // Brief pause before next round demonstration
                    self.phase = Phase::RoundSuccess {
                        until_ms: now_ms.saturating_add(600),
                        pad_off_at_ms: now_ms.saturating_add(u64::from(duration)),
                    };
                    Outcome::tone(chosen.frequency_hz(), duration)
                }
            } else {
                // More steps to repeat in this round; reset timeout
                let timeout = Self::response_timeout_ms(self.sequence.len());
                self.phase = Phase::PlayerTurn {
                    step: next_step,
                    timeout_at_ms: now_ms.saturating_add(timeout),
                    feedback_until_ms: Some(now_ms.saturating_add(u64::from(duration))),
                };
                Outcome::tone(chosen.frequency_hz(), duration)
            }
        } else {
            // Wrong choice: Game Over! Play 42 Hz square wave for 1 second.
            self.active_pad = Some(chosen);
            self.phase = Phase::GameOver {
                until_ms: now_ms.saturating_add(u64::from(RAZZ_MS)),
            };
            Outcome::tone(RAZZ_HZ, RAZZ_MS)
        }
    }
}

impl App for Simon {
    fn handle(&mut self, event: InputEvent, ctx: &Ctx) -> Outcome {
        match event {
            InputEvent::Tap { x, y } => match hit_test(x, y) {
                HitTarget::CenterHub => match self.phase {
                    Phase::Idle | Phase::GameOver { .. } | Phase::Victory { .. } => {
                        self.start_game(ctx.now_ms)
                    }
                    _ => Outcome::NONE,
                },
                HitTarget::Pad(color) => match self.phase {
                    Phase::Idle | Phase::GameOver { .. } | Phase::Victory { .. } => {
                        self.start_game(ctx.now_ms)
                    }
                    Phase::PlayerTurn { .. } => self.handle_player_choice(color, ctx.now_ms),
                    _ => Outcome::NONE,
                },
                HitTarget::None => Outcome::NONE,
            },
            InputEvent::Select => match self.phase {
                Phase::Idle | Phase::GameOver { .. } | Phase::Victory { .. } => {
                    self.start_game(ctx.now_ms)
                }
                _ => Outcome::NONE,
            },
            InputEvent::Rotate(_) => Outcome::NONE,
        }
    }

    fn tick(&mut self, ctx: &Ctx) -> Outcome {
        let now_ms = ctx.now_ms;

        match self.phase {
            Phase::Idle => Outcome::NONE,
            Phase::Demonstrating {
                step,
                is_lit,
                step_until_ms,
            } => self.tick_demonstration(step, is_lit, step_until_ms, now_ms),
            Phase::PlayerTurn {
                step,
                timeout_at_ms,
                feedback_until_ms,
            } => {
                if now_ms >= timeout_at_ms {
                    // Hesitated beyond timeout window! Game Over 42 Hz Razz.
                    self.active_pad = None;
                    self.phase = Phase::GameOver {
                        until_ms: now_ms.saturating_add(u64::from(RAZZ_MS)),
                    };
                    return Outcome::tone(RAZZ_HZ, RAZZ_MS);
                }

                if let Some(until) = feedback_until_ms
                    && now_ms >= until
                {
                    self.active_pad = None;
                    self.phase = Phase::PlayerTurn {
                        step,
                        timeout_at_ms,
                        feedback_until_ms: None,
                    };
                    return Outcome::CHANGED;
                }
                Outcome::NONE
            }
            Phase::RoundSuccess {
                until_ms,
                pad_off_at_ms,
            } => {
                if now_ms >= until_ms {
                    self.active_pad = None;
                    // Start demonstrating next sequence step
                    if self.sequence.len() < MAX_SEQUENCE {
                        let next_color = self.prng.next_color();
                        let _ = self.sequence.push(next_color);
                    }
                    let first_color = self.sequence.first().copied().unwrap_or(SimonColor::Yellow);
                    let duration = Self::tone_duration_ms(self.sequence.len());
                    self.active_pad = Some(first_color);
                    self.phase = Phase::Demonstrating {
                        step: 0,
                        is_lit: true,
                        step_until_ms: now_ms.saturating_add(u64::from(duration)),
                    };
                    Outcome::tone(first_color.frequency_hz(), duration)
                } else if now_ms >= pad_off_at_ms && self.active_pad.is_some() {
                    self.active_pad = None;
                    Outcome::CHANGED
                } else {
                    Outcome::NONE
                }
            }
            Phase::Victory { until_ms } | Phase::GameOver { until_ms } => {
                if now_ms >= until_ms && self.active_pad.is_some() {
                    self.active_pad = None;
                    Outcome::CHANGED
                } else {
                    Outcome::NONE
                }
            }
        }
    }

    fn sync(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };

        let (status, show_button, button_text, button_subtext, button_bg, button_text_color) =
            match self.phase {
                Phase::Idle => (
                    "START",
                    true,
                    "START",
                    "TAP TO PLAY",
                    slint::Color::from_rgb_u8(245, 166, 35),
                    slint::Color::from_rgb_u8(0, 0, 0),
                ),
                Phase::Demonstrating { .. } => (
                    "WATCH",
                    false,
                    "",
                    "",
                    slint::Color::from_rgb_u8(0, 0, 0),
                    slint::Color::from_rgb_u8(255, 255, 255),
                ),
                Phase::PlayerTurn { .. } => (
                    "REPEAT",
                    false,
                    "",
                    "",
                    slint::Color::from_rgb_u8(0, 0, 0),
                    slint::Color::from_rgb_u8(255, 255, 255),
                ),
                Phase::RoundSuccess { .. } => (
                    "GOOD!",
                    false,
                    "",
                    "",
                    slint::Color::from_rgb_u8(0, 0, 0),
                    slint::Color::from_rgb_u8(255, 255, 255),
                ),
                Phase::Victory { .. } => (
                    "WIN!",
                    true,
                    "YOU WIN!",
                    "PERFECT 31!",
                    slint::Color::from_rgb_u8(0, 180, 80),
                    slint::Color::from_rgb_u8(255, 255, 255),
                ),
                Phase::GameOver { .. } => (
                    "OVER",
                    true,
                    "GAME OVER",
                    "TAP TO RETRY",
                    slint::Color::from_rgb_u8(217, 20, 56),
                    slint::Color::from_rgb_u8(255, 255, 255),
                ),
            };

        shell.set_simon(SimonState {
            yellow_lit: self.active_pad == Some(SimonColor::Yellow),
            blue_lit: self.active_pad == Some(SimonColor::Blue),
            red_lit: self.active_pad == Some(SimonColor::Red),
            green_lit: self.active_pad == Some(SimonColor::Green),
            score: i32::try_from(self.score).unwrap_or(0),
            level: i32::try_from(self.level()).unwrap_or(1),
            status: status.into(),
            accent: slint::Color::from_rgb_u8(255, 215, 0),
            show_button,
            button_text: button_text.into(),
            button_subtext: button_subtext.into(),
            button_bg,
            button_text_color,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher::Feedback;

    #[test]
    fn hit_test_identifies_center_hub() {
        assert_eq!(hit_test(CENTER, CENTER), HitTarget::CenterHub);
        assert_eq!(hit_test(CENTER + 30, CENTER + 30), HitTarget::CenterHub);
        assert_eq!(hit_test(CENTER - 60, CENTER), HitTarget::CenterHub);
    }

    #[test]
    fn hit_test_identifies_four_quadrants() {
        // Yellow: Top-Left
        assert_eq!(
            hit_test(CENTER - 100, CENTER - 100),
            HitTarget::Pad(SimonColor::Yellow)
        );
        // Blue: Top-Right
        assert_eq!(
            hit_test(CENTER + 100, CENTER - 100),
            HitTarget::Pad(SimonColor::Blue)
        );
        // Red: Bottom-Left
        assert_eq!(
            hit_test(CENTER - 100, CENTER + 100),
            HitTarget::Pad(SimonColor::Red)
        );
        // Green: Bottom-Right
        assert_eq!(
            hit_test(CENTER + 100, CENTER + 100),
            HitTarget::Pad(SimonColor::Green)
        );
    }

    #[test]
    fn hit_test_rejects_outside_circular_rim() {
        assert_eq!(hit_test(0, 0), HitTarget::None);
        assert_eq!(hit_test(390, 390), HitTarget::None);
    }

    #[test]
    fn prng_generates_all_four_colors() {
        let mut prng = Prng::new(42);
        let mut seen = [false; 4];
        for _ in 0..50 {
            match prng.next_color() {
                SimonColor::Yellow => seen[0] = true,
                SimonColor::Blue => seen[1] = true,
                SimonColor::Red => seen[2] = true,
                SimonColor::Green => seen[3] = true,
            }
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn game_flow_happy_path_and_game_over() {
        let mut simon = Simon::new(slint::Weak::default());
        assert_eq!(simon.phase, Phase::Idle);

        let ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        // Tap to start
        let outcome = simon.handle(
            InputEvent::Tap {
                x: CENTER,
                y: CENTER,
            },
            &ctx,
        );
        assert!(outcome.changed);
        assert!(matches!(simon.phase, Phase::Demonstrating { .. }));
        assert_eq!(simon.sequence.len(), 1);

        let first = simon
            .sequence
            .first()
            .copied()
            .expect("initial element exists");

        // Fast-forward demonstration tone
        let mut now = 1000 + u64::from(Simon::tone_duration_ms(simon.sequence.len()));
        let _ = simon.tick(&Ctx {
            now_ms: now,
            ble_linked: false,
        });

        // Fast-forward gap
        now += u64::from(GAP_DURATION_MS);
        let _ = simon.tick(&Ctx {
            now_ms: now,
            ble_linked: false,
        });
        assert!(matches!(simon.phase, Phase::PlayerTurn { step: 0, .. }));

        // Player inputs matching color
        let (tap_x, tap_y) = match first {
            SimonColor::Yellow => (CENTER - 100, CENTER - 100),
            SimonColor::Blue => (CENTER + 100, CENTER - 100),
            SimonColor::Red => (CENTER - 100, CENTER + 100),
            SimonColor::Green => (CENTER + 100, CENTER + 100),
        };

        let outcome = simon.handle(
            InputEvent::Tap { x: tap_x, y: tap_y },
            &Ctx {
                now_ms: now,
                ble_linked: false,
            },
        );
        assert!(outcome.changed);
        assert_eq!(simon.score, 1);
        assert!(matches!(simon.phase, Phase::RoundSuccess { .. }));

        // Fast-forward round success pause to start round 2
        now += 600;
        let _ = simon.tick(&Ctx {
            now_ms: now,
            ble_linked: false,
        });
        assert_eq!(simon.sequence.len(), 2);
        assert!(matches!(simon.phase, Phase::Demonstrating { .. }));

        // Finish demonstrating round 2
        for _ in 0..4 {
            now += 450;
            let _ = simon.tick(&Ctx {
                now_ms: now,
                ble_linked: false,
            });
        }
        assert!(matches!(simon.phase, Phase::PlayerTurn { .. }));

        // Player inputs wrong color
        let wrong_color = match simon.sequence.first().copied() {
            Some(SimonColor::Yellow) => SimonColor::Blue,
            _ => SimonColor::Yellow,
        };
        let (wrong_x, wrong_y) = match wrong_color {
            SimonColor::Yellow => (CENTER - 100, CENTER - 100),
            _ => (CENTER + 100, CENTER - 100),
        };

        let outcome = simon.handle(
            InputEvent::Tap {
                x: wrong_x,
                y: wrong_y,
            },
            &Ctx {
                now_ms: now,
                ble_linked: false,
            },
        );
        assert_eq!(outcome.feedback, Some(Feedback::Tone { hz: 42, ms: 1000 }));
        assert!(matches!(simon.phase, Phase::GameOver { .. }));
        assert_eq!(simon.score, 1);
    }

    #[test]
    fn timing_intervals_match_1978_specification() {
        // Signals 1–5: 420 ms tone, 5000 ms timeout
        for len in 1..=5 {
            assert_eq!(Simon::tone_duration_ms(len), 420);
            assert_eq!(Simon::response_timeout_ms(len), 5_000);
        }
        // Signals 6–9: 320 ms tone, 3000 ms timeout
        for len in 6..=9 {
            assert_eq!(Simon::tone_duration_ms(len), 320);
            assert_eq!(Simon::response_timeout_ms(len), 3_000);
        }
        // Signals 10–13: 220 ms tone, 3000 ms timeout
        for len in 10..=13 {
            assert_eq!(Simon::tone_duration_ms(len), 220);
            assert_eq!(Simon::response_timeout_ms(len), 3_000);
        }
        // Signals 14–31: 140 ms tone, 1500 ms timeout
        for len in 14..=31 {
            assert_eq!(Simon::tone_duration_ms(len), 140);
            assert_eq!(Simon::response_timeout_ms(len), 1_500);
        }
    }

    #[test]
    fn player_turn_times_out_with_42hz_razz() {
        let mut simon = Simon::new(slint::Weak::default());
        let mut now = 10_000;
        let _ = simon.start_game(now);

        // Advance through initial demonstration
        now += u64::from(Simon::tone_duration_ms(1));
        let _ = simon.tick(&Ctx {
            now_ms: now,
            ble_linked: false,
        });
        now += u64::from(GAP_DURATION_MS);
        let _ = simon.tick(&Ctx {
            now_ms: now,
            ble_linked: false,
        });
        assert!(matches!(simon.phase, Phase::PlayerTurn { .. }));

        // Wait beyond 5.0 second timeout window
        now += 5_001;
        let outcome = simon.tick(&Ctx {
            now_ms: now,
            ble_linked: false,
        });

        assert_eq!(outcome.feedback, Some(Feedback::Tone { hz: 42, ms: 1000 }));
        assert!(matches!(simon.phase, Phase::GameOver { .. }));
    }

    #[test]
    fn completing_31_signals_wins_game() {
        let mut simon = Simon::new(slint::Weak::default());
        let now = 20_000;
        let _ = simon.start_game(now);

        // Pre-fill sequence with 31 colors
        simon.sequence.clear();
        for _ in 0..WIN_SEQUENCE {
            let _ = simon.sequence.push(SimonColor::Blue);
        }

        // Set player turn on the final 31st step (step 30)
        simon.phase = Phase::PlayerTurn {
            step: 30,
            timeout_at_ms: now + 1500,
            feedback_until_ms: None,
        };

        // Player taps 31st correct color (Blue: top-right)
        let outcome = simon.handle(
            InputEvent::Tap {
                x: CENTER + 100,
                y: CENTER - 100,
            },
            &Ctx {
                now_ms: now,
                ble_linked: false,
            },
        );

        assert!(matches!(simon.phase, Phase::Victory { .. }));
        assert_eq!(simon.score, 1);
        assert_eq!(
            outcome.feedback,
            Some(Feedback::Tone {
                hz: 165,
                ms: Simon::tone_duration_ms(31)
            })
        );
    }
}
