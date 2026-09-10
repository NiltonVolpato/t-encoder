//! Magic 8 Ball app.
//!
//! An oracle app for the round 390×390 AMOLED display.
//!
//! - Shows the classic "8" crest on load.
//! - Press the encoder or tap the screen to shake and reveal an oracle answer with haptic rumble.
//! - Rotate the rotary dial to cycle through 5 distinct themes (Classic, Existential, Sarcastic, Corporate, Debugger).
//! - Rotating flashes a white theme banner for 1.0 second.
//! - Dial rotation wraps around infinitely in both directions.

use launcher::{
    App, AppFactory, Ctx, Feedback, IconId, InputEvent, Manifest, Outcome, TouchAccess, ViewId,
};
use ui::{Magic8State, Shell};

/// Total number of available themes.
pub const THEME_COUNT: usize = 5;

/// Duration in milliseconds to display the white mode banner after rotating.
pub const BANNER_DURATION_MS: u64 = 1000;

/// Authentic 20 answers from the traditional Magic 8 Ball.
pub const CLASSIC_ANSWERS: [&str; 20] = [
    "IT IS CERTAIN",
    "IT IS DECIDEDLY SO",
    "WITHOUT A DOUBT",
    "YES DEFINITELY",
    "YOU MAY RELY ON IT",
    "AS I SEE IT, YES",
    "MOST LIKELY",
    "OUTLOOK GOOD",
    "YES",
    "SIGNS POINT TO YES",
    "REPLY HAZY, TRY AGAIN",
    "ASK AGAIN LATER",
    "BETTER NOT TELL YOU NOW",
    "CANNOT PREDICT NOW",
    "CONCENTRATE AND ASK AGAIN",
    "DON'T COUNT ON IT",
    "MY REPLY IS NO",
    "MY SOURCES SAY NO",
    "OUTLOOK NOT SO GOOD",
    "VERY DOUBTFUL",
];

/// Deadpan, existential philosophical answers.
pub const EXISTENTIAL_ANSWERS: [&str; 12] = [
    "Does it really matter?",
    "You already know the answer.",
    "The universe is indifferent.",
    "Flip a coin; notice what you hope for.",
    "Define 'good'.",
    "Entropy wins regardless.",
    "In the grand scheme, no.",
    "All paths lead to dust.",
    "Free will is an illusion.",
    "Why ask why?",
    "Silence is the only truth.",
    "You are on your own.",
];

/// Passive-aggressive, sarcastic answers.
pub const SARCASTIC_ANSWERS: [&str; 12] = [
    "Bold of you to assume.",
    "Sure, if you enjoy chaos.",
    "I wouldn't, but you do you.",
    "Ask someone who cares.",
    "Have you considered thinking first?",
    "My sources are laughing.",
    "Oh, absolutely. (Not.)",
    "Good luck with that.",
    "Are you serious right now?",
    "Don't quit your day job.",
    "I'm an 8-ball, not a miracle worker.",
    "Whatever helps you sleep at night.",
];

/// Corporate jargon and executive non-answers.
pub const CORPORATE_ANSWERS: [&str; 12] = [
    "Let's take this offline.",
    "Circle back next quarter.",
    "Action item not approved.",
    "Synergy looks promising.",
    "Not aligned with current OKRs.",
    "Per my last email, no.",
    "Let's put a pin in that.",
    "Low-hanging fruit, go for it.",
    "Bandwidth exceeded.",
    "Table this for next sprint.",
    "Hard stop. Moving on.",
    "Value proposition unclear.",
];

/// Developer & debugger quips.
pub const DEBUGGER_ANSWERS: [&str; 12] = [
    "Off by one.",
    "Check your base cases.",
    "Did you flip the condition?",
    "It's an integer overflow.",
    "Race condition. Good luck.",
    "Stale cache. Invalidate it.",
    "Works on my machine.",
    "Undefined behavior.",
    "Null pointer exception.",
    "Check the git blame.",
    "Stack overflow imminent.",
    "Have you tried turning it off and on?",
];

/// Theme configuration containing metadata, answer pool, and color scheme.
#[derive(Clone, Copy, Debug)]
pub struct ThemeConfig {
    /// Display name shown in uppercase.
    pub name: &'static str,
    /// Pool of oracle responses for this theme.
    pub answers: &'static [&'static str],
    /// Viewport background color.
    pub bg: slint::Color,
    /// Theme accent color for glowing rim and indicators.
    pub accent: slint::Color,
    /// Foreground text color.
    pub text_color: slint::Color,
}

/// The 5 Magic 8 Ball theme definitions.
pub static THEMES: [ThemeConfig; THEME_COUNT] = [
    ThemeConfig {
        name: "CLASSIC",
        answers: &CLASSIC_ANSWERS,
        bg: slint::Color::from_rgb_u8(10, 28, 66),
        accent: slint::Color::from_rgb_u8(32, 128, 255),
        text_color: slint::Color::from_rgb_u8(255, 255, 255),
    },
    ThemeConfig {
        name: "EXISTENTIAL",
        answers: &EXISTENTIAL_ANSWERS,
        bg: slint::Color::from_rgb_u8(36, 12, 54),
        accent: slint::Color::from_rgb_u8(191, 85, 236),
        text_color: slint::Color::from_rgb_u8(243, 232, 255),
    },
    ThemeConfig {
        name: "SARCASTIC",
        answers: &SARCASTIC_ANSWERS,
        bg: slint::Color::from_rgb_u8(58, 12, 12),
        accent: slint::Color::from_rgb_u8(255, 77, 77),
        text_color: slint::Color::from_rgb_u8(255, 240, 240),
    },
    ThemeConfig {
        name: "CORPORATE",
        answers: &CORPORATE_ANSWERS,
        bg: slint::Color::from_rgb_u8(8, 38, 52),
        accent: slint::Color::from_rgb_u8(0, 210, 211),
        text_color: slint::Color::from_rgb_u8(224, 247, 250),
    },
    ThemeConfig {
        name: "DEBUGGER",
        answers: &DEBUGGER_ANSWERS,
        bg: slint::Color::from_rgb_u8(5, 36, 12),
        accent: slint::Color::from_rgb_u8(0, 255, 102),
        text_color: slint::Color::from_rgb_u8(57, 255, 20),
    },
];

/// Computes wrap-around theme index given current index and signed encoder delta.
#[must_use]
pub fn wrap_theme_index(current: usize, delta: i32) -> usize {
    let count = i32::try_from(THEME_COUNT).unwrap_or(5);
    let step = delta.rem_euclid(count);
    let cur: i32 = i32::try_from(current).unwrap_or_default();
    let next = cur.saturating_add(step).rem_euclid(count);
    usize::try_from(next).unwrap_or(0)
}

/// Compact Xorshift32 PRNG for on-chip random oracle selection.
#[derive(Clone, Copy, Debug)]
pub struct Prng {
    state: u32,
}

impl Prng {
    /// Creates a PRNG with a non-zero initial seed.
    #[must_use]
    pub const fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x85eb_ca6b } else { seed },
        }
    }

    /// Generates the next pseudo-random 32-bit integer.
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Selects a pseudo-random index in `0..len`.
    pub fn next_index(&mut self, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let r = self.next_u32();
        let len_u32 = u32::try_from(len).unwrap_or(1);
        let idx = r.checked_rem(len_u32).unwrap_or(0);
        usize::try_from(idx).unwrap_or(0)
    }
}

/// Builds [`Magic8`] instances for the launcher router.
pub struct Magic8Factory {
    manifest: Manifest,
    shell: slint::Weak<Shell>,
}

impl Magic8Factory {
    /// Creates a new factory against the shared Slint tree.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> Self {
        Self {
            manifest: Manifest {
                name: "Magic Ball",
                icon: IconId(3),
                view: ViewId(4),
                accent: embedded_graphics::pixelcolor::Rgb565::new(0, 36, 31),
                touch: TouchAccess::Taps,
                requires_ble: false,
            },
            shell,
        }
    }
}

impl AppFactory for Magic8Factory {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> alloc::boxed::Box<dyn App + '_> {
        alloc::boxed::Box::new(Magic8::new(self.shell.clone()))
    }
}

/// Running instance of the Magic 8 Ball app.
pub struct Magic8 {
    shell: slint::Weak<Shell>,
    prng: Prng,
    theme_index: usize,
    current_answer: &'static str,
    banner_until_ms: u64,
    is_shaken: bool,
    seeded: bool,
}

impl Magic8 {
    /// Creates a fresh Magic 8 Ball instance in initial load screen state.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> Self {
        Self {
            shell,
            prng: Prng::new(0),
            theme_index: 0,
            current_answer: "",
            banner_until_ms: 0,
            is_shaken: false,
            seeded: false,
        }
    }

    /// Shakes the 8-ball: picks a random prophecy and produces haptic feedback.
    fn shake(&mut self, now_ms: u64) -> Outcome {
        if !self.seeded && now_ms != 0 {
            let max_u64 = u64::from(u32::MAX);
            let rem = now_ms.checked_rem(max_u64).unwrap_or(1);
            let seed = u32::try_from(rem).unwrap_or(1);
            self.prng = Prng::new(seed);
            self.seeded = true;
        }

        // Clear any active mode banner immediately.
        self.banner_until_ms = 0;

        let theme = THEMES
            .get(self.theme_index)
            .or_else(|| THEMES.first())
            .unwrap_or(&THEMES[0]);
        let idx = self.prng.next_index(theme.answers.len());
        self.current_answer = theme.answers.get(idx).copied().unwrap_or("");
        self.is_shaken = true;

        Outcome::buzz(Feedback::Haptic)
    }

    /// Rotates the dial: switches theme, resets to load state, and shows mode banner for 1s.
    fn rotate(&mut self, delta: i32, now_ms: u64) -> Outcome {
        if delta == 0 {
            return Outcome::NONE;
        }

        self.theme_index = wrap_theme_index(self.theme_index, delta);
        self.banner_until_ms = now_ms.saturating_add(BANNER_DURATION_MS);
        self.is_shaken = false;

        Outcome::buzz(Feedback::Beep)
    }

    /// Returns the currently active theme configuration.
    #[must_use]
    pub fn current_theme(&self) -> &ThemeConfig {
        THEMES.get(self.theme_index).unwrap_or(&THEMES[0])
    }

    /// Whether the mode banner is currently visible.
    #[must_use]
    pub fn is_showing_banner(&self, now_ms: u64) -> bool {
        self.banner_until_ms > 0 && now_ms < self.banner_until_ms
    }

    /// Active theme index (0..4).
    #[must_use]
    pub const fn theme_index(&self) -> usize {
        self.theme_index
    }

    /// Active answer text.
    #[must_use]
    pub const fn answer(&self) -> &'static str {
        self.current_answer
    }

    /// Whether the ball has been shaken.
    #[must_use]
    pub const fn is_shaken(&self) -> bool {
        self.is_shaken
    }
}

impl App for Magic8 {
    fn handle(&mut self, event: InputEvent, ctx: &Ctx) -> Outcome {
        match event {
            InputEvent::Rotate(delta) => self.rotate(delta, ctx.now_ms),
            InputEvent::Select | InputEvent::Tap { .. } => self.shake(ctx.now_ms),
            InputEvent::Swipe(_) | InputEvent::HoldProgress(_) | InputEvent::Hold => Outcome::NONE,
        }
    }

    fn tick(&mut self, ctx: &Ctx) -> Outcome {
        if self.banner_until_ms > 0 && ctx.now_ms >= self.banner_until_ms {
            self.banner_until_ms = 0;
            return Outcome::CHANGED;
        }
        Outcome::NONE
    }

    fn sync(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };

        let theme = self.current_theme();
        let mode_index = i32::try_from(self.theme_index.saturating_add(1)).unwrap_or(1);
        let mode_total = i32::try_from(THEME_COUNT).unwrap_or(5);

        shell.set_magic8(Magic8State {
            mode_name: theme.name.into(),
            mode_index,
            mode_total,
            theme_bg: theme.bg,
            theme_accent: theme.accent,
            text_color: theme.text_color,
            answer_text: self.current_answer.into(),
            is_banner: self.banner_until_ms > 0,
            is_shaken: self.is_shaken,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_theme_index_forward_and_backward() {
        assert_eq!(wrap_theme_index(0, 1), 1);
        assert_eq!(wrap_theme_index(1, 1), 2);
        assert_eq!(wrap_theme_index(2, 1), 3);
        assert_eq!(wrap_theme_index(3, 1), 4);
        assert_eq!(wrap_theme_index(4, 1), 0); // wrap forward

        assert_eq!(wrap_theme_index(0, -1), 4); // wrap backward
        assert_eq!(wrap_theme_index(4, -1), 3);
        assert_eq!(wrap_theme_index(3, -1), 2);
        assert_eq!(wrap_theme_index(2, -1), 1);
        assert_eq!(wrap_theme_index(1, -1), 0);

        // Multiple steps
        assert_eq!(wrap_theme_index(0, 5), 0);
        assert_eq!(wrap_theme_index(0, 6), 1);
        assert_eq!(wrap_theme_index(0, -6), 4);
    }

    #[test]
    fn test_initial_state_shows_load_screen() {
        let app = Magic8::new(slint::Weak::default());
        assert_eq!(app.theme_index(), 0);
        assert!(!app.is_shaken());
        assert_eq!(app.answer(), "");
        assert!(!app.is_showing_banner(0));
    }

    #[test]
    fn test_rotation_triggers_banner_and_resets_shaken() {
        let mut app = Magic8::new(slint::Weak::default());
        let ctx = Ctx {
            now_ms: 1000,
            ble_linked: false,
        };

        // Shake first
        let _ = app.handle(InputEvent::Select, &ctx);
        assert!(app.is_shaken());

        // Rotate
        let outcome = app.handle(InputEvent::Rotate(1), &ctx);
        assert_eq!(outcome.feedback, Some(Feedback::Beep));
        assert_eq!(app.theme_index(), 1);
        assert!(!app.is_shaken());
        assert!(app.is_showing_banner(1000));
        assert!(app.is_showing_banner(1500));
        assert!(!app.is_showing_banner(2000));
    }

    #[test]
    fn test_banner_expires_on_tick() {
        let mut app = Magic8::new(slint::Weak::default());
        let ctx = Ctx {
            now_ms: 500,
            ble_linked: false,
        };

        let _ = app.handle(InputEvent::Rotate(1), &ctx);
        assert!(app.is_showing_banner(500));

        // Tick before expiration
        let ctx_mid = Ctx {
            now_ms: 1200,
            ble_linked: false,
        };
        let mid_outcome = app.tick(&ctx_mid);
        assert_eq!(mid_outcome, Outcome::NONE);

        // Tick after expiration (500 + 1000 = 1500ms)
        let ctx_after = Ctx {
            now_ms: 1500,
            ble_linked: false,
        };
        let after_outcome = app.tick(&ctx_after);
        assert_eq!(after_outcome, Outcome::CHANGED);
        assert!(!app.is_showing_banner(1500));
    }

    #[test]
    fn test_shake_reveals_answer_with_haptic() {
        let mut app = Magic8::new(slint::Weak::default());
        let ctx = Ctx {
            now_ms: 2500,
            ble_linked: false,
        };

        let outcome = app.handle(InputEvent::Select, &ctx);
        assert_eq!(outcome.feedback, Some(Feedback::Haptic));
        assert!(app.is_shaken());
        assert!(!app.answer().is_empty());
        assert!(CLASSIC_ANSWERS.contains(&app.answer()));
    }

    #[test]
    fn test_tap_also_shakes() {
        let mut app = Magic8::new(slint::Weak::default());
        let ctx = Ctx {
            now_ms: 3000,
            ble_linked: false,
        };

        let outcome = app.handle(InputEvent::Tap { x: 195, y: 195 }, &ctx);
        assert_eq!(outcome.feedback, Some(Feedback::Haptic));
        assert!(app.is_shaken());
        assert!(!app.answer().is_empty());
    }

    #[test]
    fn test_prng_generates_valid_indices_for_all_themes() {
        let mut prng = Prng::new(12345);
        for theme in &THEMES {
            for _ in 0..100 {
                let idx = prng.next_index(theme.answers.len());
                assert!(idx < theme.answers.len());
                let ans = theme.answers.get(idx);
                assert!(ans.is_some());
            }
        }
    }
}
