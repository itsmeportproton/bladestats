//! The settings file, shared by the overlay and the configurator.
//!
//! Two programs read and write this: the overlay applies it, the configurator edits it. It is
//! also plain TOML that anyone can open in a text editor, which is the reason for most of the
//! decisions here.
//!
//! **A broken config never stops the overlay.** Unreadable, malformed, half-written — any
//! of those fall back to defaults and log why. An overlay that refuses to start because one
//! line has a typo is worse than an overlay showing default settings.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::Theme;

/// Where the overlay sits on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "Top left",
            Corner::TopRight => "Top right",
            Corner::BottomLeft => "Bottom left",
            Corner::BottomRight => "Bottom right",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    pub corner: Corner,
    /// Gap from the screen edge, in pixels.
    pub margin: f32,
    pub font_size: f32,
    /// Redraws per second. Higher costs the game more and reads no better.
    pub refresh_hz: u32,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            corner: Corner::TopLeft,
            margin: 32.0,
            font_size: 16.0,
            refresh_hz: 10,
        }
    }
}

impl Placement {
    /// Clamps to values the overlay can actually honour.
    ///
    /// Applied on load rather than trusted, because this file is hand-editable: a font size of
    /// zero or a refresh rate of 10000 would otherwise produce an invisible overlay or one
    /// that eats the frame budget it exists to measure.
    pub fn sanitised(mut self) -> Self {
        self.font_size = self.font_size.clamp(8.0, 48.0);
        self.margin = self.margin.clamp(0.0, 512.0);
        self.refresh_hz = self.refresh_hz.clamp(1, 60);
        self
    }
}

/// Which readings the overlay draws. Every one of these corresponds to a row or a column in
/// the HUD.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    pub fps: bool,
    pub frame_time: bool,
    pub low_1pct: bool,
    pub low_01pct: bool,

    pub cpu_name: bool,
    pub cpu_load: bool,
    pub cpu_cores: bool,
    pub cpu_clock: bool,
    pub cpu_temp: bool,
    pub cpu_power: bool,

    pub gpu_name: bool,
    pub gpu_load: bool,
    pub gpu_clock: bool,
    pub gpu_vram: bool,
    pub gpu_temp: bool,
    /// The hottest point on the die, beside the edge reading. Off by default: it is the more
    /// useful of the two but also the more alarming-looking, and a card is meant to run there.
    pub gpu_hotspot: bool,
    pub gpu_fan: bool,
    pub gpu_power: bool,

    pub ram_usage: bool,
    pub ram_spec: bool,
}

impl Default for Metrics {
    /// Everything a machine can generally read is on. The ones off by default are the ones
    /// that are usually blank anyway: a 0.1% low needs a thousand frames before it means
    /// anything, and processor temperature has no sensor path that avoids a kernel driver.
    fn default() -> Self {
        Self {
            fps: true,
            frame_time: true,
            low_1pct: true,
            low_01pct: false,

            cpu_name: true,
            cpu_load: true,
            cpu_cores: true,
            cpu_clock: true,
            cpu_temp: false,
            cpu_power: true,

            gpu_name: true,
            gpu_load: true,
            gpu_clock: true,
            gpu_vram: true,
            gpu_temp: true,
            gpu_hotspot: false,
            gpu_fan: false,
            gpu_power: true,

            ram_usage: true,
            ram_spec: true,
        }
    }
}

/// Readings inferred from how a game presents rather than read from a driver.
///
/// Kept apart from [`Metrics`] because they carry a different promise. These will be wrong
/// sometimes and can break when a driver changes, so they are off unless asked for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Experimental {
    /// Which graphics API the game renders with.
    pub graphics_api: bool,
    /// Whether frame generation is running, and how many frames it adds.
    pub generated_frames: bool,
    /// Render resolution against output resolution, when the game is upscaling.
    pub render_scale: bool,
    /// Live memory transfer rate. Needs vendor access that has no documented user-mode path.
    pub ram_live_rate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Shows and hides the overlay.
    pub toggle: String,
    /// Re-reads this file without restarting.
    pub reload: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            toggle: "Ctrl+Alt+B".into(),
            reload: "Ctrl+Alt+R".into(),
        }
    }
}

/// The whole settings file.
///
/// Deliberately **not** `deny_unknown_fields`. A key this build does not recognise is ignored
/// rather than fatal, so a file written by a newer version still opens in an older one instead
/// of dropping the user back to defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub placement: Placement,
    pub theme: Theme,
    pub metrics: Metrics,
    pub experimental: Experimental,
    pub behaviour: Behaviour,
    pub sensors: Sensors,
    pub hotkeys: Hotkeys,
}

/// Where the processor's temperature is allowed to come from.
///
/// An enumeration rather than a switch, so a signed driver — should this project ever have one
/// — becomes another variant rather than a settings migration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CpuTempSource {
    /// Not read at all. The default, and deliberately so: every source involves a program that
    /// has loaded a kernel driver, and that is the user's decision to make knowingly.
    #[default]
    Off,
    /// Whichever hardware monitor is running and answering.
    Auto,
}

/// Readings that need something outside this program.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sensors {
    pub cpu_temp: CpuTempSource,
    /// Port LibreHardwareMonitor serves its sensor tree on.
    #[serde(default = "default_lhm_port")]
    pub lhm_port: u16,
}

fn default_lhm_port() -> u16 {
    8085
}

/// When the overlay shows itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Behaviour {
    /// Keep the panel out of the way until a game is on screen.
    ///
    /// On by default: an overlay is for games, and on a desktop it is clutter. The toggle
    /// hotkey overrides it either way, which is what makes leaving it on safe — a game the
    /// detection misses costs one keypress rather than a broken-looking program.
    pub only_in_games: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            only_in_games: true,
        }
    }
}

/// What happened while loading, so the caller can tell the user rather than guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// Read and parsed.
    Loaded,
    /// No file yet; defaults are in use. Normal on a first run.
    Missing,
    /// The file exists but could not be parsed. Defaults are in use and the text explains why.
    Invalid(String),
}

impl Config {
    /// Reads the config, falling back to defaults on any problem.
    ///
    /// Returns the outcome alongside the config so the caller can surface a parse error in the
    /// overlay itself. A message in a log file helps nobody who is mid-game.
    pub fn load(path: &Path) -> (Self, LoadOutcome) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), LoadOutcome::Missing);
            }
            Err(e) => return (Self::default(), LoadOutcome::Invalid(e.to_string())),
        };

        match toml::from_str::<Config>(&text) {
            Ok(config) => (config.sanitised(), LoadOutcome::Loaded),
            Err(e) => (
                Self::default(),
                // The first line carries the useful part; the rest is a source excerpt that
                // does not fit in an overlay.
                LoadOutcome::Invalid(e.message().lines().next().unwrap_or("bad TOML").into()),
            ),
        }
    }

    /// Writes the config, creating the parent directory if needed.
    ///
    /// Writes to a temporary file and renames it into place. A half-written config is exactly
    /// the kind of file that would be read back as garbage, and the overlay may be reading
    /// this while the configurator saves it.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;

        let temp = path.with_extension("toml.tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, path)
    }

    fn sanitised(mut self) -> Self {
        self.placement = self.placement.sanitised();
        self
    }
}

/// The settings file location.
///
/// Next to the executable, because bladestats is meant to be unpacked and run: settings should
/// travel with it on a stick and leave nothing behind. When that directory is not writable
/// — Program Files, a read-only share — it falls back to the user's roaming profile so
/// saving still works.
pub fn default_path() -> PathBuf {
    const FILE: &str = "bladestats.toml";

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && is_writable(dir)
    {
        return dir.join(FILE);
    }

    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("bladestats").join(FILE),
        None => PathBuf::from(FILE),
    }
}

/// Tests writability by actually writing, since permissions on Windows cannot be inferred
/// reliably from metadata alone.
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".bladestats-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Color;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bladestats-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_round_trip_through_toml() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();

        assert_eq!(back.placement.font_size, 16.0);
        assert!(back.metrics.fps);
        assert!(!back.experimental.graphics_api);
        assert_eq!(back.hotkeys.toggle, "Ctrl+Alt+B");
    }

    #[test]
    fn a_missing_file_yields_defaults_rather_than_an_error() {
        let (config, outcome) = Config::load(&temp_dir().join("does-not-exist.toml"));
        assert_eq!(outcome, LoadOutcome::Missing);
        assert!(config.metrics.fps, "defaults should still be usable");
    }

    #[test]
    fn a_broken_file_falls_back_to_defaults_and_explains_itself() {
        let path = temp_dir().join("broken.toml");
        std::fs::write(&path, "this is not = = toml [[[").unwrap();

        let (config, outcome) = Config::load(&path);
        match outcome {
            LoadOutcome::Invalid(message) => assert!(!message.is_empty(), "needs a reason"),
            other => panic!("expected an invalid outcome, got {other:?}"),
        }
        // The overlay has to keep working; refusing to start over a typo would be worse.
        assert!(config.metrics.fps);
    }

    #[test]
    fn a_partial_file_keeps_defaults_for_everything_it_omits() {
        let path = temp_dir().join("partial.toml");
        std::fs::write(&path, "[placement]\nfont_size = 22.0\n").unwrap();

        let (config, outcome) = Config::load(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(config.placement.font_size, 22.0);
        assert_eq!(
            config.placement.refresh_hz, 10,
            "untouched keys keep defaults"
        );
        assert!(config.metrics.fps, "whole missing sections keep defaults");
    }

    #[test]
    fn an_unrecognised_key_is_ignored_rather_than_fatal() {
        // A file written by a newer build must still open here, with the keys this build
        // understands intact.
        let path = temp_dir().join("future.toml");
        std::fs::write(
            &path,
            "[placement]\nfont_size = 20.0\nquantum_flux = true\n\n[from_the_future]\nx = 1\n",
        )
        .unwrap();

        let (config, outcome) = Config::load(&path);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(config.placement.font_size, 20.0);
    }

    #[test]
    fn hand_edited_nonsense_is_clamped_to_something_the_overlay_can_draw() {
        let path = temp_dir().join("silly.toml");
        std::fs::write(
            &path,
            "[placement]\nfont_size = 0.0\nrefresh_hz = 10000\nmargin = -50.0\n",
        )
        .unwrap();

        let (config, _) = Config::load(&path);
        assert!(
            config.placement.font_size >= 8.0,
            "an invisible overlay is not a setting"
        );
        assert!(
            config.placement.refresh_hz <= 60,
            "redrawing faster than the screen costs the game frames for nothing"
        );
        assert!(config.placement.margin >= 0.0);
    }

    #[test]
    fn saving_then_loading_preserves_every_change() {
        let path = temp_dir().join("round-trip.toml");

        let mut config = Config::default();
        config.placement.corner = Corner::BottomRight;
        config.placement.font_size = 19.0;
        config.metrics.low_01pct = true;
        config.metrics.cpu_temp = false;
        config.experimental.graphics_api = true;
        config.theme.vendor_colors = false;
        config.theme.background = Color::rgba(0, 0, 0, 0x40);
        config.hotkeys.toggle = "Ctrl+Shift+M".into();

        config.save(&path).unwrap();
        let (back, outcome) = Config::load(&path);

        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(back.placement.corner, Corner::BottomRight);
        assert_eq!(back.placement.font_size, 19.0);
        assert!(back.metrics.low_01pct);
        assert!(!back.metrics.cpu_temp);
        assert!(back.experimental.graphics_api);
        assert!(!back.theme.vendor_colors);
        assert_eq!(back.theme.background.a, 0x40);
        assert_eq!(back.hotkeys.toggle, "Ctrl+Shift+M");
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let path = temp_dir().join("clean.toml");
        Config::default().save(&path).unwrap();

        assert!(path.exists());
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "the temporary file must be renamed, not left next to the real one"
        );
    }

    #[test]
    fn the_default_path_is_a_toml_file_with_a_parent() {
        let path = default_path();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("toml"));
        assert!(path.parent().is_some());
    }

    #[test]
    fn corners_all_have_labels() {
        for corner in Corner::ALL {
            assert!(!corner.label().is_empty());
        }
    }
}
