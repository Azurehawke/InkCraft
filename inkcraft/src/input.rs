//! Pointer input: a bounded event queue and Catmull-Rom stroke smoothing.
//!
//! Windowing events are pushed into a bounded `crossbeam-channel` and drained
//! once per frame. Pushing never blocks; if the queue is full the sample is
//! counted in [`InputConsumer::dropped`] instead of stalling the event loop.

use crate::model::StrokePoint;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use winit::event::Force;

/// Maximum queued events between frames.
pub const QUEUE_CAPACITY: usize = 16_384;
/// Line segments generated per pair of input samples.
pub const SMOOTHING_STEPS: usize = 8;
/// Samples closer than this to the previous accepted one are discarded (page units).
pub const MIN_SAMPLE_DISTANCE: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

/// A pointer sample in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerSample {
    pub phase: Phase,
    pub screen: [f64; 2],
    pub pressure: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    Pointer(PointerSample),
    /// Drag delta in screen pixels.
    Pan { dx: f64, dy: f64 },
    /// Zoom multiplier around a screen-space anchor.
    Zoom { factor: f64, anchor: [f64; 2] },
}

/// Producer half, held by the windowing event handler.
#[derive(Clone)]
pub struct InputProducer {
    tx: Sender<InputEvent>,
    dropped: Arc<AtomicU64>,
}

impl InputProducer {
    pub fn push(&self, event: InputEvent) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(event) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Consumer half, drained by the frame loop.
pub struct InputConsumer {
    rx: Receiver<InputEvent>,
    dropped: Arc<AtomicU64>,
}

impl InputConsumer {
    pub fn drain(&self) -> impl Iterator<Item = InputEvent> + '_ {
        self.rx.try_iter()
    }

    /// Number of events discarded because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

pub fn input_channel() -> (InputProducer, InputConsumer) {
    let (tx, rx) = bounded(QUEUE_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        InputProducer {
            tx,
            dropped: dropped.clone(),
        },
        InputConsumer { rx, dropped },
    )
}

/// Maps a winit touch force to normalized pressure. Missing force defaults to full pressure.
pub fn pressure_from_force(force: Option<Force>) -> f32 {
    match force {
        Some(Force::Normalized(p)) => p as f32,
        Some(Force::Calibrated {
            force,
            max_possible_force,
            ..
        }) if max_possible_force > 0.0 => (force / max_possible_force) as f32,
        _ => 1.0,
    }
}

/// Output of [`StrokeSmoother::finish`].
#[derive(Debug)]
pub struct FinishedStroke {
    /// Accepted raw samples, to be stored in the document.
    pub raw: Vec<StrokePoint>,
    /// Smoothed polyline for the whole stroke.
    pub smoothed: Vec<StrokePoint>,
}

/// Incremental Catmull-Rom smoother.
///
/// A segment between samples `k` and `k+1` needs sample `k+2` as its tangent
/// control point, so the newest segment is emitted one sample late. That is
/// one pen sample of latency, a few milliseconds at typical report rates.
#[derive(Debug, Default)]
pub struct StrokeSmoother {
    raw: Vec<StrokePoint>,
    smoothed: Vec<StrokePoint>,
    next_segment: usize,
}

impl StrokeSmoother {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts a sample. Returns `false` if it was discarded as a near-duplicate.
    pub fn push(&mut self, p: StrokePoint) -> bool {
        match self.raw.last() {
            None => self.smoothed.push(p),
            Some(last) => {
                let dx = p.x - last.x;
                let dy = p.y - last.y;
                if dx * dx + dy * dy < MIN_SAMPLE_DISTANCE * MIN_SAMPLE_DISTANCE {
                    return false;
                }
            }
        }
        self.raw.push(p);
        while self.next_segment + 2 < self.raw.len() {
            self.emit_segment();
        }
        true
    }

    /// Smoothed points emitted so far. Lags the newest accepted sample by one segment.
    pub fn smoothed(&self) -> &[StrokePoint] {
        &self.smoothed
    }

    /// Flushes the tail segments, clamping the final tangent to the last sample.
    pub fn finish(mut self) -> FinishedStroke {
        while self.next_segment + 1 < self.raw.len() {
            self.emit_segment();
        }
        FinishedStroke {
            raw: self.raw,
            smoothed: self.smoothed,
        }
    }

    fn emit_segment(&mut self) {
        let k = self.next_segment;
        let n = self.raw.len();
        let p0 = self.raw[k.saturating_sub(1)];
        let p1 = self.raw[k];
        let p2 = self.raw[k + 1];
        let p3 = self.raw[(k + 2).min(n - 1)];
        for step in 1..=SMOOTHING_STEPS {
            let t = step as f32 / SMOOTHING_STEPS as f32;
            self.smoothed.push(catmull_rom(p0, p1, p2, p3, t));
        }
        self.next_segment += 1;
    }
}

/// Smooths a complete set of raw samples. Produces the same polyline the live
/// smoother emits for those samples, so rendering is deterministic.
pub fn smooth(raw: &[StrokePoint]) -> Vec<StrokePoint> {
    let mut smoother = StrokeSmoother::new();
    for &p in raw {
        smoother.push(p);
    }
    smoother.finish().smoothed
}

fn catmull_rom(p0: StrokePoint, p1: StrokePoint, p2: StrokePoint, p3: StrokePoint, t: f32) -> StrokePoint {
    let t2 = t * t;
    let t3 = t2 * t;
    let eval = |a: f32, b: f32, c: f32, d: f32| {
        0.5 * ((2.0 * b)
            + (-a + c) * t
            + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
            + (-a + 3.0 * b - 3.0 * c + d) * t3)
    };
    StrokePoint {
        x: eval(p0.x, p1.x, p2.x, p3.x),
        y: eval(p0.y, p1.y, p2.y, p3.y),
        pressure: eval(p0.pressure, p1.pressure, p2.pressure, p3.pressure).clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> StrokePoint {
        StrokePoint { x, y, pressure: 1.0 }
    }

    #[test]
    fn smoothing_passes_through_samples_and_endpoints() {
        let raw = [pt(0.0, 0.0), pt(10.0, 5.0), pt(20.0, 0.0), pt(30.0, 5.0)];
        let out = smooth(&raw);
        assert_eq!(out.len(), (raw.len() - 1) * SMOOTHING_STEPS + 1);
        assert_eq!(out[0], raw[0]);
        let last = out.last().unwrap();
        assert!((last.x - 30.0).abs() < 1e-4 && (last.y - 5.0).abs() < 1e-4);
        // Each original interior sample is reached exactly at its segment boundary.
        assert!((out[SMOOTHING_STEPS].x - 10.0).abs() < 1e-4);
        assert!((out[SMOOTHING_STEPS].y - 5.0).abs() < 1e-4);
    }

    #[test]
    fn straight_input_stays_straight() {
        let raw: Vec<_> = (0..6).map(|i| pt(i as f32 * 3.0, 7.0)).collect();
        for p in smooth(&raw) {
            assert!((p.y - 7.0).abs() < 1e-4);
        }
    }

    #[test]
    fn near_duplicate_samples_are_dropped() {
        let mut s = StrokeSmoother::new();
        assert!(s.push(pt(0.0, 0.0)));
        assert!(!s.push(pt(0.1, 0.1)));
        assert!(s.push(pt(5.0, 0.0)));
        assert_eq!(s.finish().raw.len(), 2);
    }

    #[test]
    fn live_output_is_prefix_of_offline_output() {
        let raw: Vec<_> = (0..20)
            .map(|i| pt(i as f32 * 2.0, (i as f32).sin() * 4.0))
            .collect();
        let mut live = StrokeSmoother::new();
        for &p in &raw {
            live.push(p);
        }
        let partial = live.smoothed().to_vec();
        let full = smooth(&raw);
        assert_eq!(&full[..partial.len()], &partial[..]);
    }

    #[test]
    fn pressure_mapping() {
        assert_eq!(pressure_from_force(None), 1.0);
        assert_eq!(pressure_from_force(Some(Force::Normalized(0.25))), 0.25);
        let calibrated = Force::Calibrated {
            force: 2.0,
            max_possible_force: 4.0,
            altitude_angle: None,
        };
        assert_eq!(pressure_from_force(Some(calibrated)), 0.5);
    }

    #[test]
    fn queue_counts_drops_instead_of_blocking() {
        let (tx, rx) = input_channel();
        for _ in 0..QUEUE_CAPACITY + 10 {
            tx.push(InputEvent::Pan { dx: 0.0, dy: 0.0 });
        }
        assert_eq!(rx.dropped(), 10);
        assert_eq!(rx.drain().count(), QUEUE_CAPACITY);
    }
}
