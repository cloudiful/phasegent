//! The progress surface of one run.
//!
//! The progress is a human affordance: it runs only for a human caller on an
//! interactive terminal, in the text mode, outside CI, and never for a machine
//! output. The ticker is an indeterminate spinner on stderr, and the stages
//! come from the real operation boundaries, so no percentage and no total is
//! ever invented.
//!
//! The guard owns the ticker: dropping it clears the line and stops the
//! ticker, so an early return, an error, or a panic never leaves a ticker
//! behind. The stage updates and the clear before a result or a diagnostic
//! reach the running ticker through the thread-local slot, so the deep
//! operation boundaries can report without threading a handle through every
//! layer.

use std::cell::RefCell;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::output::context::{OutputContext, OutputMode};

/// The draw rate of the ticker: about ten frames per second.
const REFRESH_HZ: u8 = 10;
/// The interval of the steady tick.
const TICK_INTERVAL: Duration = Duration::from_millis(100);
/// The spinner glyphs of a Unicode terminal.
const UNICODE_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// The spinner glyphs of a terminal without Unicode support.
const ASCII_TICKS: &[&str] = &["-", "\\", "|", "/"];

thread_local! {
    static ACTIVE: RefCell<Option<ProgressBar>> = const { RefCell::new(None) };
}

/// The progress of the running command.
pub struct ProgressGuard {
    bar: Option<ProgressBar>,
}

impl ProgressGuard {
    /// Start the progress of one command.
    ///
    /// The ticker only runs when the run allows it; every other case gets a
    /// hidden sink whose stages are dropped, so a machine output never
    /// carries a progress line.
    pub fn start(context: &OutputContext, message: &str) -> Self {
        let bar = if allowed(context) {
            let bar = ProgressBar::new_spinner();
            bar.set_draw_target(ProgressDrawTarget::stderr_with_hz(REFRESH_HZ));
            bar.set_style(spinner_style());
            bar.set_message(sanitize(message));
            bar.enable_steady_tick(TICK_INTERVAL);
            Some(bar)
        } else {
            None
        };
        publish(bar.clone());
        Self { bar }
    }

    /// Update the current stage of this progress.
    #[allow(dead_code)] // the operation boundaries report through `stage`
    pub fn update(&self, message: &str) {
        if let Some(bar) = &self.bar {
            bar.set_message(sanitize(message));
        }
    }

    /// Clear the ticker before a result or a diagnostic.
    pub fn clear(&self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
    }

    /// Start a progress with an injected draw target. The lifecycle test uses
    /// it to observe the drawn frames without a terminal.
    #[cfg(test)]
    pub(crate) fn start_with_target(message: &str, target: ProgressDrawTarget) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_draw_target(target);
        bar.set_style(spinner_style());
        bar.set_message(sanitize(message));
        publish(Some(bar.clone()));
        Self { bar: Some(bar) }
    }

    /// Draw one frame. The lifecycle test uses it to observe the ticker
    /// without waiting for the steady tick.
    #[cfg(test)]
    pub(crate) fn tick(&self) {
        if let Some(bar) = &self.bar {
            bar.tick();
        }
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.clear();
        publish(None);
    }
}

/// Whether the run may show a progress surface.
///
/// The gate reads the *effective* mode, so a command whose legacy format
/// selection overrides the global resolution (`worktree prune --json` under an
/// automatic text default) never animates, and a legacy text selection on a
/// machine default may. It also needs a human caller, an interactive terminal
/// on both streams, no CI, a terminal that supports cursor control, and no
/// `--no-progress`. An agent role never gets an animation, even with an
/// explicit text selection.
pub fn allowed(context: &OutputContext) -> bool {
    context.effective_mode() == OutputMode::Text
        && context.human_caller
        && context.stdout_is_terminal
        && context.stderr_is_terminal
        && !context.ci_active
        && context.term_capable
        && context.progress
}

/// Update the stage of the active progress. A stage outside a progress run is
/// dropped.
pub fn stage(message: &str) {
    ACTIVE.with(|active| {
        if let Some(bar) = active.borrow().as_ref() {
            bar.set_message(sanitize(message));
        }
    });
}

/// Whether a progress ticker is running in this thread.
#[allow(dead_code)] // read by the lifecycle test
pub fn active() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}

/// Clear the active ticker before a result or a diagnostic.
pub fn clear() {
    ACTIVE.with(|active| {
        if let Some(bar) = active.borrow().as_ref() {
            bar.finish_and_clear();
        }
    });
}

/// Publish the ticker of the running command.
fn publish(bar: Option<ProgressBar>) {
    ACTIVE.with(|active| *active.borrow_mut() = bar);
}

/// The spinner style of the terminal.
fn spinner_style() -> ProgressStyle {
    let style = ProgressStyle::default_spinner();
    if unicode_supported() {
        style.tick_strings(UNICODE_TICKS)
    } else {
        style.tick_strings(ASCII_TICKS)
    }
}

/// Whether the terminal environment supports Unicode glyphs.
fn unicode_supported() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .any(|value| {
            let value = value.to_ascii_uppercase();
            value.contains("UTF-8") || value.contains("UTF8")
        })
}

/// The stage text of the progress. The same sanitizing the text output uses
/// applies, so a stage can never carry a terminal control sequence.
fn sanitize(message: &str) -> String {
    crate::output::render::sanitize(message)
}
