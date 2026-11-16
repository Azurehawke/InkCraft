//! Ruler tick spacing and guide snapping. Pure functions, unit tested without a window.

/// Tick spacings in page units, fine to coarse.
const STEPS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];

/// Smallest gap between minor ruler ticks, in screen pixels.
const MIN_TICK_PX: f64 = 10.0;

/// Every this many ticks gets a label.
pub const LABEL_EVERY: i64 = 5;

/// Tick spacing in page units for a zoom level (screen pixels per page unit).
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

/// Whether the tick at `value` gets a label.
pub fn is_labeled(value: f64, step: f64) -> bool {
    (value / step).round() as i64 % LABEL_EVERY == 0
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
        assert_eq!(tick_step(1.0), 10.0);
        assert_eq!(tick_step(4.0), 5.0);
        assert_eq!(tick_step(0.1), 100.0);
        assert_eq!(tick_step(0.001), 1000.0);
    }

    #[test]
    fn ticks_cover_the_range_inclusively() {
        let t: Vec<f64> = ticks(-3.0, 25.0, 10.0).collect();
        assert_eq!(t, vec![0.0, 10.0, 20.0]);
        assert_eq!(ticks(11.0, 12.0, 10.0).count(), 0);
    }

    #[test]
    fn every_fifth_tick_is_labeled() {
        assert!(is_labeled(0.0, 10.0));
        assert!(is_labeled(50.0, 10.0));
        assert!(!is_labeled(10.0, 10.0));
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
