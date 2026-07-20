//! Turning readings into the strings the overlay draws.
//!
//! Every one of these returns the dash for a reading that could not be taken, which is the
//! rule the whole snapshot layer is built on: "zero watts" and "watts unknown" are different
//! facts, and a UI that renders them the same shows the user plausible-looking fiction.

use bs_core::Power;

/// What fills a metric that could not be read.
pub const MISSING: &str = "—";

pub fn pct(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0}"))
}

/// Clocks in gigahertz rather than megahertz.
///
/// The design asks for `5.21 GHz`, and it is right to: four significant figures of megahertz
/// is more precision than a boost clock deserves, and the number changes every sample, so the
/// extra digits are noise that draws the eye.
pub fn ghz(mhz: Option<f32>) -> String {
    mhz.map_or(MISSING.into(), |m| format!("{:.2}", m / 1000.0))
}

pub fn temp(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0}"))
}

pub fn opt_fps(v: Option<f32>) -> String {
    v.map_or(MISSING.into(), |v| format!("{v:.0}"))
}

/// Watts, tagged with their provenance: a tilde means a derived estimate, not a sensor
/// reading.
pub fn watts(p: Option<Power>) -> String {
    match p {
        None => MISSING.into(),
        Some(p) if p.is_estimated() => format!("~{:.0}", p.watts()),
        Some(p) => format!("{:.0}", p.watts()),
    }
}

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// `11.2 / 16.0`, without the unit — the unit is a separate span in a dimmer colour.
pub fn pair_gb(used: Option<u64>, total: Option<u64>) -> String {
    match (used, total) {
        (Some(u), Some(t)) => format!("{:.1} / {:.1}", u as f64 / GB, t as f64 / GB),
        (Some(u), None) => format!("{:.1}", u as f64 / GB),
        _ => MISSING.into(),
    }
}

/// How wide a `used / total` pair can ever get, in characters.
///
/// Taken from the total, because that is the fixed half: the panel then has room for the used
/// half at its widest and never has to move to make some. Without a total there is nothing to
/// reserve against, so the value gets whatever it needs.
pub fn pair_gb_reserve(total: Option<u64>) -> usize {
    total.map_or(0, |t| pair_gb(Some(t), Some(t)).chars().count())
}

/// How full something is, 0.0..=1.0, or `None` when either end is unknown.
///
/// A bar drawn from a guessed total would be worse than no bar: it looks like a measurement.
pub fn fraction(used: Option<u64>, total: Option<u64>) -> Option<f32> {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => Some((u as f32 / t as f32).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// `DDR5-5800`, the way a memory kit is named on its own box.
pub fn memory_name(kind: Option<&str>, rated_mhz: Option<u32>) -> Option<String> {
    match (kind, rated_mhz) {
        (Some(k), Some(r)) => Some(format!("{k}-{r}")),
        (Some(k), None) => Some(k.to_string()),
        _ => None,
    }
}

/// `32768 MB · 2 × 16384 · rated 5800 MT/s`.
///
/// Returns nothing rather than a partial line: half of this sentence is not informative, it
/// is just clutter along the bottom of the panel.
pub fn memory_spec(modules: &[u32], rated_mhz: Option<u32>) -> Option<String> {
    if modules.is_empty() {
        return None;
    }
    let total: u32 = modules.iter().sum();
    let mut spec = format!("{total} MB");

    // Only worth spelling out the arrangement when every module matches, which is the case
    // worth knowing about: a mismatched pair is described by listing it, not by "2 ×".
    let first = modules[0];
    if modules.iter().all(|m| *m == first) {
        spec.push_str(&format!(" · {} × {first}", modules.len()));
    } else {
        let each: Vec<String> = modules.iter().map(|m| m.to_string()).collect();
        spec.push_str(&format!(" · {}", each.join(" + ")));
    }

    if let Some(rated) = rated_mhz {
        spec.push_str(&format!(" · rated {rated} MT/s"));
    }
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unread_metric_becomes_a_dash_and_never_a_zero() {
        assert_eq!(pct(None), MISSING);
        assert_eq!(ghz(None), MISSING);
        assert_eq!(temp(None), MISSING);
        assert_eq!(watts(None), MISSING);
        assert_eq!(pair_gb(None, None), MISSING);
    }

    #[test]
    fn estimated_watts_are_marked_with_a_tilde_and_measured_ones_are_not() {
        assert_eq!(watts(Some(Power::Estimated(65.0))), "~65");
        assert_eq!(watts(Some(Power::Measured(145.0))), "145");
    }

    #[test]
    fn clocks_read_in_gigahertz_to_two_places() {
        assert_eq!(ghz(Some(5210.0)), "5.21");
        assert_eq!(ghz(Some(400.0)), "0.40");
    }

    #[test]
    fn a_bar_needs_both_ends_before_it_will_draw() {
        assert_eq!(fraction(Some(8), Some(16)), Some(0.5));
        assert_eq!(fraction(Some(8), None), None, "a guessed total is not a total");
        assert_eq!(fraction(Some(8), Some(0)), None, "nothing is 100% of nothing");
        // Reported usage can momentarily exceed the total; the bar must not run off its track.
        assert_eq!(fraction(Some(20), Some(16)), Some(1.0));
    }

    #[test]
    fn memory_is_named_the_way_its_box_names_it() {
        assert_eq!(
            memory_name(Some("DDR5"), Some(5800)),
            Some("DDR5-5800".into())
        );
        assert_eq!(memory_name(None, Some(5800)), None);
    }

    #[test]
    fn a_matched_kit_is_described_as_a_multiple_and_a_mismatched_one_is_listed() {
        assert_eq!(
            memory_spec(&[16384, 16384], Some(5800)).unwrap(),
            "32768 MB · 2 × 16384 · rated 5800 MT/s"
        );
        // "2 × 24576" would be a lie about what is in the machine.
        assert_eq!(
            memory_spec(&[16384, 8192], None).unwrap(),
            "24576 MB · 16384 + 8192"
        );
        assert_eq!(memory_spec(&[], Some(5800)), None);
    }
}
