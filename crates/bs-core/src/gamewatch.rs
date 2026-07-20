//! Deciding whether what is on screen is a game.
//!
//! The overlay is for games, and on a desktop it is clutter. But the two mistakes here are not
//! equal: a panel that appears over a browser is an annoyance, while a panel that fails to
//! appear over a game looks like a program that does not work. Everything below is shaped by
//! that asymmetry — slow to conclude a game has started, slower still to conclude one has
//! stopped, and backed by a hotkey for when it is wrong anyway.
//!
//! Two signals, and both must hold. The window in front covers its whole monitor without
//! decorations, and it is presenting frames steadily. Either alone is common: a maximised
//! editor covers the monitor and presents nothing, a browser presents plenty while scrolling
//! and does not cover anything. Together they are rare outside of games — a video played
//! fullscreen is the honest false positive, and it is one the user can live with.
//!
//! Processor load was considered as a third signal and left out. A compile, an archive
//! extraction and a video export all produce the same spike, while a light game held at its
//! refresh rate produces almost none: it fires when nothing is happening and stays quiet when
//! something is.

/// Frames per second the window in front must sustain.
///
/// Low on purpose. Loading screens, menus and paused games all present slowly, and none of
/// them are moments to make the panel vanish.
const MIN_FPS: f32 = 8.0;

/// How long the signals must hold before the panel appears.
///
/// Long enough that alt-tabbing across a fullscreen window on the way somewhere else does not
/// flash the overlay on and off behind it.
const ENTER_AFTER_NS: u64 = 1_500_000_000;

/// How long they must fail before it goes away again.
///
/// Much longer than the delay to appear. A game that stops presenting for a moment — a level
/// load, a cutscene handoff, a shader compile — has not stopped being a game, and blinking
/// the panel out and back in would be worse than leaving it up.
const LEAVE_AFTER_NS: u64 = 5_000_000_000;

/// Whether the overlay is wanted, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presence {
    /// Nothing that looks like a game.
    #[default]
    Away,
    /// A game is on screen.
    Playing,
}

/// Watches for a game starting and stopping.
#[derive(Debug, Default)]
pub struct GameWatch {
    /// When the signals first held continuously.
    qualified_from: Option<u64>,
    /// When they last held at all.
    qualified_at: Option<u64>,
    presence: Presence,
}

impl GameWatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn presence(&self) -> Presence {
        self.presence
    }

    /// Feeds in what is currently on screen and returns the conclusion.
    ///
    /// `borderless` is whether the window in front covers its monitor with no decorations;
    /// `fps` is what that window is presenting at, or `None` when nothing is being measured —
    /// which, without administrator rights, is always.
    pub fn update(&mut self, now_ns: u64, borderless: bool, fps: Option<f32>) -> Presence {
        let qualifies = borderless && fps.is_some_and(|f| f >= MIN_FPS);

        if qualifies {
            self.qualified_at = Some(now_ns);
            let from = *self.qualified_from.get_or_insert(now_ns);
            if now_ns.saturating_sub(from) >= ENTER_AFTER_NS {
                self.presence = Presence::Playing;
            }
        } else {
            self.qualified_from = None;
            let gone_for = self
                .qualified_at
                .map_or(u64::MAX, |at| now_ns.saturating_sub(at));
            if gone_for >= LEAVE_AFTER_NS {
                self.presence = Presence::Away;
            }
        }

        self.presence
    }

    /// Forgets everything — when the window in front changes to a different process.
    ///
    /// Without this, tabbing from one game to another would carry the first one's history into
    /// the second, and the panel would be up before the second had presented anything.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: u64 = 1_000_000_000;

    /// Runs `seconds` of the same conditions, a tenth of a second at a time.
    fn hold(watch: &mut GameWatch, from_ns: u64, seconds: f64, borderless: bool, fps: Option<f32>) -> u64 {
        let mut now = from_ns;
        let end = from_ns + (seconds * SEC as f64) as u64;
        while now < end {
            now += SEC / 10;
            watch.update(now, borderless, fps);
        }
        now
    }

    #[test]
    fn a_game_has_to_hold_still_for_a_moment_before_the_panel_appears() {
        let mut w = GameWatch::new();
        // Instantly qualifying would flash the overlay over any fullscreen window that
        // happened to be passed through on the way somewhere else.
        assert_eq!(w.update(SEC, true, Some(144.0)), Presence::Away);
        let t = hold(&mut w, SEC, 1.0, true, Some(144.0));
        assert_eq!(w.presence(), Presence::Away, "one second is too soon");

        hold(&mut w, t, 1.0, true, Some(144.0));
        assert_eq!(w.presence(), Presence::Playing);
    }

    #[test]
    fn a_browser_never_qualifies_however_much_it_presents() {
        let mut w = GameWatch::new();
        // Scrolling a page presents as fast as a game does. What it does not do is cover the
        // monitor without decorations.
        hold(&mut w, 0, 30.0, false, Some(144.0));
        assert_eq!(w.presence(), Presence::Away);
    }

    #[test]
    fn a_maximised_window_that_draws_nothing_never_qualifies_either() {
        let mut w = GameWatch::new();
        hold(&mut w, 0, 30.0, true, None);
        assert_eq!(w.presence(), Presence::Away);
        hold(&mut w, 30 * SEC, 30.0, true, Some(0.5));
        assert_eq!(w.presence(), Presence::Away);
    }

    #[test]
    fn a_loading_screen_does_not_make_the_panel_disappear() {
        let mut w = GameWatch::new();
        let t = hold(&mut w, 0, 3.0, true, Some(144.0));
        assert_eq!(w.presence(), Presence::Playing);

        // Four seconds of nothing being presented: a level load, a shader compile, a cutscene
        // handing over. The game has not stopped being a game.
        let t = hold(&mut w, t, 4.0, true, None);
        assert_eq!(w.presence(), Presence::Playing);

        hold(&mut w, t, 3.0, true, None);
        assert_eq!(w.presence(), Presence::Away, "but eventually it has gone");
    }

    #[test]
    fn leaving_takes_longer_than_arriving() {
        // The asymmetry is the whole design: appearing late is a nuisance, vanishing early
        // looks broken.
        assert!(LEAVE_AFTER_NS > ENTER_AFTER_NS * 2);
    }

    #[test]
    fn switching_to_another_game_starts_the_reckoning_again() {
        let mut w = GameWatch::new();
        let t = hold(&mut w, 0, 3.0, true, Some(144.0));
        assert_eq!(w.presence(), Presence::Playing);

        w.reset();
        assert_eq!(w.presence(), Presence::Away);
        assert_eq!(
            w.update(t, true, Some(144.0)),
            Presence::Away,
            "the new window has to earn it on its own"
        );
    }

    #[test]
    fn a_brief_alt_tab_away_and_back_keeps_the_panel_up() {
        let mut w = GameWatch::new();
        let t = hold(&mut w, 0, 3.0, true, Some(144.0));

        // Half a second on the desktop and straight back.
        let t = hold(&mut w, t, 0.5, false, None);
        assert_eq!(w.presence(), Presence::Playing);
        hold(&mut w, t, 0.5, true, Some(144.0));
        assert_eq!(w.presence(), Presence::Playing);
    }
}
