//! R-tree index over stroke bounding boxes for viewport culling.

use crate::model::{Rect, Stroke};
use rstar::{RTree, RTreeObject, AABB};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    id: usize,
    envelope: AABB<[f64; 2]>,
}

impl RTreeObject for Entry {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

/// Maps stroke indices to their bounding boxes. Query cost depends on the
/// number of hits, not on the total number of strokes in the notebook.
pub struct StrokeIndex {
    tree: RTree<Entry>,
}

impl Default for StrokeIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl StrokeIndex {
    pub fn new() -> Self {
        Self { tree: RTree::new() }
    }

    /// Builds the index in one pass. Ids are positions in `strokes`.
    pub fn from_strokes(strokes: &[Stroke]) -> Self {
        let entries = strokes
            .iter()
            .enumerate()
            .map(|(id, s)| Entry {
                id,
                envelope: to_aabb(s.bounds()),
            })
            .collect();
        Self {
            tree: RTree::bulk_load(entries),
        }
    }

    pub fn insert(&mut self, id: usize, bounds: Rect) {
        self.tree.insert(Entry {
            id,
            envelope: to_aabb(bounds),
        });
    }

    /// Ids of strokes whose bounds intersect `area`.
    pub fn query(&self, area: Rect) -> impl Iterator<Item = usize> + '_ {
        self.tree
            .locate_in_envelope_intersecting(to_aabb(area))
            .map(|e| e.id)
    }

    pub fn len(&self) -> usize {
        self.tree.size()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.size() == 0
    }
}

fn to_aabb(r: Rect) -> AABB<[f64; 2]> {
    AABB::from_corners(r.min, r.max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StrokePoint;

    fn stroke_at(x: f32, y: f32) -> Stroke {
        Stroke {
            color: [0, 0, 0, 255],
            width: 1.0,
            points: vec![StrokePoint { x, y, pressure: 1.0 }],
        }
    }

    #[test]
    fn query_returns_only_visible_strokes() {
        let strokes: Vec<_> = (0..1000)
            .map(|i| stroke_at((i % 100) as f32 * 10.0, (i / 100) as f32 * 10.0))
            .collect();
        let index = StrokeIndex::from_strokes(&strokes);
        assert_eq!(index.len(), 1000);

        let viewport = Rect::new([0.0, 0.0], [95.0, 45.0]);
        let mut hits: Vec<usize> = index.query(viewport).collect();
        hits.sort_unstable();
        let expected: Vec<usize> = (0..1000)
            .filter(|&i| {
                strokes[i].bounds().intersects(&viewport)
            })
            .collect();
        assert_eq!(hits, expected);
        assert!(hits.len() < 100);
    }

    #[test]
    fn insert_then_query() {
        let mut index = StrokeIndex::new();
        assert!(index.is_empty());
        index.insert(7, Rect::new([0.0, 0.0], [10.0, 10.0]));
        assert_eq!(index.query(Rect::new([5.0, 5.0], [6.0, 6.0])).collect::<Vec<_>>(), vec![7]);
        assert_eq!(index.query(Rect::new([50.0, 50.0], [60.0, 60.0])).count(), 0);
    }
}
