//! Adapter turning any upstream `enc_ui::Screen` into a launcher [`App`].
//!
//! Upstream's screens already have the shape we want — `&mut self` for input,
//! `&self` for render — so one generic wrapper adopts all of them. That the
//! adapter is this small is the useful signal: the `App` trait composes with
//! code written before it existed, rather than demanding everything be
//! rewritten against it.

use enc_ui::{Dirty, InputEvent, RenderCtx, Screen};
use launcher::{App, Canvas, Ctx, Manifest, Outcome};

/// Wraps an `enc_ui::Screen` as a launcher app.
pub struct ScreenApp<S: Screen> {
    manifest: Manifest,
    screen: S,
}

impl<S: Screen> ScreenApp<S> {
    /// Wraps `screen`, presenting it with `manifest`.
    pub const fn new(manifest: Manifest, screen: S) -> ScreenApp<S> {
        ScreenApp { manifest, screen }
    }
}

/// Builds the upstream render context from ours.
///
/// Upstream screens want a wall clock; ours carries uptime plus shared state,
/// and the epoch is only known after an SNTP sync — so this is `None` until the
/// network sets the time, which the screens already render as `--:--:--`.
fn render_ctx<'a>(ctx: &Ctx<'a>) -> RenderCtx<'a> {
    let uptime_secs = u32::try_from(ctx.now_ms / 1_000).unwrap_or(0);
    RenderCtx {
        state: ctx.state,
        hms: ctx.state.current_epoch(uptime_secs).map(enc_state::hms),
    }
}

impl<S: Screen> App for ScreenApp<S> {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn handle(&mut self, event: InputEvent, ctx: &Ctx<'_>) -> Outcome {
        Outcome::dirty(self.screen.handle(event, ctx.state))
    }

    fn tick(&mut self, ctx: &Ctx<'_>) -> Dirty {
        let render_ctx = render_ctx(ctx);
        self.screen.tick(&render_ctx)
    }

    fn render(&self, ctx: &Ctx<'_>, fb: &mut Canvas<'_>) {
        let render_ctx = render_ctx(ctx);
        // The framebuffer's error type is `Infallible`, so this cannot fail.
        let _ = self.screen.render(&render_ctx, fb);
    }
}
