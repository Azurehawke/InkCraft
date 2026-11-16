//! Ruler tick spacing and guide snapping. Pure functions, unit tested without a window.

/// Points per inch. Rulers are measured in inches, and coordinates are stored in points.
pub const POINTS_PER_INCH: f64 = 72.0;

/// Tick spacings in points: 1/8, 1/4, 1/2, 1, 2, 5 and 10 inches, fine to coarse.
const STEPS: [f64; 7] = [9.0, 18.0, 36.0, 72.0, 144.0, 360.0, 720.0];

/// Smallest gap between minor ruler ticks, in screen pixels.
const MIN_TICK_PX: f64 = 10.0;

/// Tick spacing in points for a zoom level (screen pixels per point).
pub fn tick_step(zoom: f64) -> f64 {
    STEPS
        .iter()
        .copied()
        .find(|step| step * zoom >= MIN_TICK_PX)
        .unwrap_or(STEPS[STEPS.len() - 1])
}

/// Tick positions in `from..=to`, which are multiples of `step`.
pub fn ticks(from: f64, to: f64, step: f64) -> impl Iterator<Item = f64> {
    let first = (from / step).ceil() as i64;
    let last = (to / step).floor() as i64;
    (first..=last).map(move |n| n as f64 * step)
}

/// Whether the tick at `value` (in points) is a whole inch, which gets a label.
pub fn is_labeled(value: f64) -> bool {
    value % POINTS_PER_INCH == 0.0
}

/// A position in points written in inches: `72` becomes `1`, and `36` becomes `0.5`.
pub fn format_inches(points: f64) -> String {
    let inches = points / POINTS_PER_INCH;
    if inches.fract() == 0.0 {
        return format!("{}", inches as i64);
    }
    let text = format!("{inches:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The guide position nearest to `value`, if one lies within `tolerance`.
pub fn snap(value: f64, guides: impl IntoIterator<Item = f64>, tolerance: f64) -> Option<f64> {
    guides
        .into_iter()
        .filter(|g| (g - value).abs() <= tolerance)
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_grows_as_you_zoom_out() {
        assert_eq!(tick_step(4.0), 9.0);
        assert_eq!(tick_step(1.0), 18.0);
        assert_eq!(tick_step(0.1), 144.0);
        assert_eq!(tick_step(0.0001), 720.0);
    }

    #[test]
    fn whole_inches_are_labeled_in_inches() {
        assert!(is_labeled(0.0));
        assert!(is_labeled(144.0));
        assert!(!is_labeled(18.0));
        assert_eq!(format_inches(144.0), "2");
        assert_eq!(format_inches(36.0), "0.5");
        assert_eq!(format_inches(9.0), "0.125");
        assert_eq!(format_inches(-72.0), "-1");
    }

    #[test]
    fn ticks_cover_the_range_inclusively() {
        let t: Vec<f64> = ticks(-3.0, 25.0, 10.0).collect();
        assert_eq!(t, vec![0.0, 10.0, 20.0]);
        assert_eq!(ticks(11.0, 12.0, 10.0).count(), 0);
    }

    #[test]
    fn snap_picks_the_nearest_guide_in_tolerance() {
        let guides = [100.0, 104.0, 300.0];
        assert_eq!(snap(102.0, guides, 5.0), Some(100.0));
        assert_eq!(snap(103.0, guides, 5.0), Some(104.0));
        assert_eq!(snap(200.0, guides, 5.0), None);
        assert_eq!(snap(1.0, Vec::<f64>::new(), 5.0), None);
    }
}
