//! The editable stroke list with its spatial index and undo history.
//!
//! Every change is an [`Edit`] that knows how to reverse itself, so strokes can
//! be added, erased, and restored through one code path. Stroke ids are
//! positions in [`Document::strokes`]; they shift whenever strokes are removed
//! or restored, so callers must not hold ids across an edit.

use std::sync::Arc;

use crate::model::{PageImage, Rect, Stroke, StrokePoint};
use crate::spatial::StrokeIndex;

/// Strokes can extend this far beyond their points (half the widest nib).
/// Hit queries widen the search box by this much so no stroke is missed.
const MAX_HALF_WIDTH: f64 = 40.0;

#[derive(Clone, Debug)]
enum Edit {
    /// Put these strokes back at these ids, in ascending id order.
    Insert(Vec<(usize, Stroke)>),
    /// Take the strokes at these ids out. The ids are the positions before removal.
    Remove(Vec<(usize, Stroke)>),
    /// Put these images back at these ids, in ascending id order.
    AddImages(Vec<(usize, Arc<PageImage>)>),
    /// Take the images at these ids out, as [`Edit::Remove`] does for strokes.
    RemoveImages(Vec<(usize, Arc<PageImage>)>),
    /// Move image `index` from `from` to `to`.
    MoveImage { index: usize, from: Rect, to: Rect },
}

pub struct Document {
    strokes: Vec<Stroke>,
    /// Imported pages and templates, drawn under the strokes. Each can be moved and scaled.
    images: Vec<Arc<PageImage>>,
    index: StrokeIndex,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Bounds of everything touched since the last [`Document::take_dirty`].
    dirty: Vec<Rect>,
}

impl Document {
    pub fn new(strokes: Vec<Stroke>) -> Self {
        Self::with_images(strokes, Vec::new())
    }

    pub fn with_images(strokes: Vec<Stroke>, images: Vec<PageImage>) -> Self {
        let images = images.into_iter().map(Arc::new).collect();
        let index = StrokeIndex::from_strokes(&strokes);
        Self {
            strokes,
            images,
            index,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: Vec::new(),
        }
    }

    pub fn strokes(&self) -> &[Stroke] {
        &self.strokes
    }

    pub fn index(&self) -> &StrokeIndex {
        &self.index
    }

    pub fn images(&self) -> &[Arc<PageImage>] {
        &self.images
    }

    /// Appends imported images as one undoable edit.
    pub fn add_images(&mut self, images: Vec<PageImage>) {
        if images.is_empty() {
            return;
        }
        let first = self.images.len();
        let items = images.into_iter().enumerate().map(|(i, im)| (first + i, Arc::new(im))).collect();
        self.apply(Edit::AddImages(items));
    }

    /// Removes image `index` as one undoable edit.
    pub fn remove_image(&mut self, index: usize) {
        if let Some(image) = self.images.get(index).cloned() {
            self.apply(Edit::RemoveImages(vec![(index, image)]));
        }
    }

    /// Moves image `index` to `rect` while a drag is under way, without an undo step.
    /// [`Document::commit_image_move`] records the whole drag once it ends.
    pub fn preview_image(&mut self, index: usize, rect: Rect) {
        if let Some(image) = self.images.get_mut(index) {
            self.dirty.push(image.rect);
            self.dirty.push(rect);
            Arc::make_mut(image).rect = rect;
        }
    }

    /// Records a finished drag of image `index` from `from` to `to` as one undo step. The
    /// image is already at `to` (see [`Document::preview_image`]).
    pub fn commit_image_move(&mut self, index: usize, from: Rect, to: Rect) {
        if from != to && index < self.images.len() {
            self.undo.push(Edit::MoveImage { index, from: to, to: from });
            self.redo.clear();
        }
    }

    /// Appends a stroke as a new undoable edit. Returns its id.
    pub fn commit(&mut self, stroke: Stroke) -> usize {
        let id = self.strokes.len();
        self.apply(Edit::Insert(vec![(id, stroke)]));
        id
    }

    /// Removes the strokes with these ids as one undoable edit.
    pub fn erase(&mut self, ids: &[usize]) {
        let mut sorted: Vec<usize> = ids.iter().copied().filter(|&i| i < self.strokes.len()).collect();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.is_empty() {
            return;
        }
        let items = sorted.iter().map(|&i| (i, self.strokes[i].clone())).collect();
        self.apply(Edit::Remove(items));
    }

    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        let inverse = self.perform(edit);
        self.redo.push(inverse);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        let inverse = self.perform(edit);
        self.undo.push(inverse);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Returns and clears the bounds of everything changed since the last call.
    pub fn take_dirty(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.dirty)
    }

    /// Ids of strokes whose points lie within `radius` of `p` (page units),
    /// taking each stroke's width into account. Ascending order.
    pub fn hit(&self, p: [f64; 2], radius: f64) -> Vec<usize> {
        let margin = radius + MAX_HALF_WIDTH;
        let area = Rect::new([p[0] - margin, p[1] - margin], [p[0] + margin, p[1] + margin]);
        let mut ids: Vec<usize> = self
            .index
            .query(area)
            .filter(|&id| {
                let s = &self.strokes[id];
                distance_to_stroke(s, p) <= radius + s.width as f64 / 2.0
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Ids of strokes whose bounds lie entirely inside `area`. Ascending order.
    pub fn within(&self, area: Rect) -> Vec<usize> {
        let mut ids: Vec<usize> = self
            .index
            .query(area)
            .filter(|&id| {
                let b = self.strokes[id].bounds();
                b.min[0] >= area.min[0]
                    && b.min[1] >= area.min[1]
                    && b.max[0] <= area.max[0]
                    && b.max[1] <= area.max[1]
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    fn apply(&mut self, edit: Edit) {
        let inverse = self.perform(edit);
        self.undo.push(inverse);
        self.redo.clear();
    }

    /// Carries out `edit` and returns the edit that undoes it.
    fn perform(&mut self, edit: Edit) -> Edit {
        match edit {
            Edit::Insert(items) => {
                // Ascending order: each insert lands at its final position.
                for (id, stroke) in &items {
                    self.dirty.push(stroke.bounds());
                    self.strokes.insert(*id, stroke.clone());
                }
                self.reindex();
                Edit::Remove(items)
            }
            Edit::Remove(items) => {
                // Descending order keeps the remaining ids valid.
                for (id, _) in items.iter().rev() {
                    let stroke = self.strokes.remove(*id);
                    self.dirty.push(stroke.bounds());
                }
                self.reindex();
                Edit::Insert(items)
            }
            Edit::AddImages(items) => {
                for (id, image) in &items {
                    self.dirty.push(image.rect);
                    self.images.insert(*id, image.clone());
                }
                Edit::RemoveImages(items)
            }
            Edit::RemoveImages(items) => {
                for (id, _) in items.iter().rev() {
                    let image = self.images.remove(*id);
                    self.dirty.push(image.rect);
                }
                Edit::AddImages(items)
            }
            Edit::MoveImage { index, from, to } => {
                if let Some(image) = self.images.get_mut(index) {
                    self.dirty.push(image.rect);
                    self.dirty.push(to);
                    Arc::make_mut(image).rect = to;
                }
                Edit::MoveImage { index, from: to, to: from }
            }
        }
    }

    fn reindex(&mut self) {
        self.index = StrokeIndex::from_strokes(&self.strokes);
    }
}

/// Shortest distance from `p` to the stroke's polyline. A single point is its own polyline.
pub fn distance_to_stroke(stroke: &Stroke, p: [f64; 2]) -> f64 {
    let pts: &[StrokePoint] = &stroke.points;
    match pts {
        [] => f64::MAX,
        [only] => dist(p, [only.x as f64, only.y as f64]),
        _ => pts
            .windows(2)
            .map(|w| segment_distance(p, [w[0].x as f64, w[0].y as f64], [w[1].x as f64, w[1].y as f64]))
            .fold(f64::MAX, f64::min),
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 == 0.0 {
        return dist(p, a);
    }
    let t = (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0);
    dist(p, [a[0] + ab[0] * t, a[1] + ab[1] * t])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x0: f32, y0: f32, x1: f32, y1: f32) -> Stroke {
        Stroke {
            color: [0, 0, 0, 255],
            width: 2.0,
            points: vec![
                StrokePoint { x: x0, y: y0, pressure: 1.0 },
                StrokePoint { x: x1, y: y1, pressure: 1.0 },
            ],
        }
    }

    #[test]
    fn commit_undo_redo_round_trip() {
        let mut doc = Document::new(vec![]);
        doc.commit(line(0.0, 0.0, 10.0, 0.0));
        doc.commit(line(0.0, 20.0, 10.0, 20.0));
        assert_eq!(doc.strokes().len(), 2);
        assert!(doc.undo());
        assert_eq!(doc.strokes().len(), 1);
        assert!(doc.redo());
        assert_eq!(doc.strokes().len(), 2);
        assert!(!doc.redo());
    }

    #[test]
    fn erase_then_undo_restores_original_order() {
        let mut doc = Document::new(vec![
            line(0.0, 0.0, 1.0, 0.0),
            line(0.0, 10.0, 1.0, 10.0),
            line(0.0, 20.0, 1.0, 20.0),
        ]);
        let before: Vec<f32> = doc.strokes().iter().map(|s| s.points[0].y).collect();
        doc.erase(&[0, 2]);
        assert_eq!(doc.strokes().len(), 1);
        assert_eq!(doc.strokes()[0].points[0].y, 10.0);
        assert!(doc.undo());
        let after: Vec<f32> = doc.strokes().iter().map(|s| s.points[0].y).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn index_stays_consistent_after_removals() {
        let mut doc = Document::new(vec![line(0.0, 0.0, 1.0, 0.0), line(100.0, 0.0, 101.0, 0.0)]);
        doc.erase(&[0]);
        // The remaining stroke is now id 0; a hit near x=100 must find it, not a stale id.
        assert_eq!(doc.hit([100.5, 0.0], 3.0), vec![0]);
        assert!(doc.hit([0.5, 0.0], 3.0).is_empty());
    }

    #[test]
    fn hit_respects_radius_and_width() {
        let doc = Document::new(vec![line(0.0, 0.0, 100.0, 0.0)]);
        assert_eq!(doc.hit([50.0, 1.5], 1.0), vec![0]); // 1.5 is within radius 1 + half width 1
        assert!(doc.hit([50.0, 10.0], 1.0).is_empty());
    }

    #[test]
    fn within_requires_full_containment() {
        let doc = Document::new(vec![line(10.0, 10.0, 20.0, 20.0), line(90.0, 90.0, 300.0, 300.0)]);
        let area = Rect::new([0.0, 0.0], [100.0, 100.0]);
        assert_eq!(doc.within(area), vec![0]);
    }

    #[test]
    fn dirty_bounds_are_reported_once() {
        let mut doc = Document::new(vec![]);
        doc.commit(line(0.0, 0.0, 5.0, 5.0));
        assert_eq!(doc.take_dirty().len(), 1);
        assert!(doc.take_dirty().is_empty());
    }

    #[test]
    fn image_moves_and_imports_undo_and_redo() {
        let page = |y: f64| PageImage { rect: Rect::new([0.0, y], [100.0, y + 50.0]), data: vec![1] };
        let mut doc = Document::with_images(vec![], vec![page(0.0)]);
        doc.add_images(vec![page(100.0)]);
        assert_eq!(doc.images().len(), 2);

        let from = doc.images()[0].rect;
        let to = Rect::new([10.0, 10.0], [210.0, 110.0]);
        doc.preview_image(0, to);
        doc.commit_image_move(0, from, to);
        assert_eq!(doc.images()[0].rect, to);

        assert!(doc.undo());
        assert_eq!(doc.images()[0].rect, from);
        assert!(doc.redo());
        assert_eq!(doc.images()[0].rect, to);

        assert!(doc.undo());
        assert!(doc.undo());
        assert_eq!(doc.images().len(), 1);
        doc.remove_image(0);
        assert!(doc.images().is_empty());
        assert!(doc.undo());
        assert_eq!(doc.images().len(), 1);
    }
}
