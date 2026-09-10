use std::collections::HashMap;
use std::f64::consts::TAU;

use crate::geometry::{Arc, CELL, Grid, Trace, footprint, turn};

#[derive(Default)]
pub(super) struct Ground {
    paths: Vec<Path>,
    cells: HashMap<(i32, i32), Vec<usize>>,
}

struct Path {
    from: (f64, f64),
    to: (f64, f64),
    curve: Option<((f64, f64), f64, f64, f64)>,
    length: f64,
    rise: f64,
}

impl Path {
    fn new(from: (f64, f64), to: (f64, f64), arc: Option<Arc>, rise: f64) -> Option<Self> {
        if footprint::cells(Grid::default(), from, to, arc, |_| {}) == Trace::Refused {
            return None;
        }
        let curve = arc.and_then(|arc| turn(from, to, arc));
        let length = footprint::along(from, to, arc);
        Some(Self {
            from,
            to,
            curve,
            length,
            rise,
        })
    }

    fn at(&self, share: f64) -> (f64, f64) {
        match self.curve {
            Some((centre, radius, start, sweep)) => {
                let angle = start + sweep * share;
                (
                    centre.0 + radius * angle.cos(),
                    centre.1 + radius * angle.sin(),
                )
            }
            None => (
                self.from.0 + (self.to.0 - self.from.0) * share,
                self.from.1 + (self.to.1 - self.from.1) * share,
            ),
        }
    }

    fn distance(&self, point: (f64, f64)) -> f64 {
        let distance = |at: (f64, f64)| (point.0 - at.0).hypot(point.1 - at.1);
        if let Some((centre, radius, start, sweep)) = self.curve {
            let angle = (point.1 - centre.1).atan2(point.0 - centre.0);
            let turned = if sweep < 0.0 {
                start - angle
            } else {
                angle - start
            }
            .rem_euclid(TAU);
            if turned <= sweep.abs() {
                return (distance(centre) - radius).abs();
            }
            return distance(self.from).min(distance(self.to));
        }
        let direction = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let squared = direction.0 * direction.0 + direction.1 * direction.1;
        let share = if squared > 0.0 {
            (((point.0 - self.from.0) * direction.0 + (point.1 - self.from.1) * direction.1)
                / squared)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        distance(self.at(share))
    }

    fn normal(&self, share: f64) -> (f64, f64) {
        match self.curve {
            Some((_, _, start, sweep)) => {
                let angle = start + sweep * share;
                (angle.cos(), angle.sin())
            }
            None if self.length > 0.0 => (
                (self.from.1 - self.to.1) / self.length,
                (self.to.0 - self.from.0) / self.length,
            ),
            _ => (0.0, 0.0),
        }
    }
}

impl Ground {
    fn across(&self, path: &Path, share: f64, reach: f64) -> f64 {
        let point = path.at(share);
        let normal = path.normal(share);
        let count = ((reach * 2.0 / Grid::FINEST).ceil() as usize).max(1);
        let height = |index: usize| {
            let sideways = reach * (2.0 * (index as f64 + 0.5) / count as f64 - 1.0);
            self.at(
                (point.0 + normal.0 * sideways, point.1 + normal.1 * sideways),
                reach,
            )
        };
        let first = height(0);
        first + (1..count).map(|index| height(index) - first).sum::<f64>() / count as f64
    }

    pub(super) fn profile(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        arc: Option<Arc>,
        reach: f64,
    ) -> Vec<(f64, f64)> {
        if self.paths.is_empty() {
            return vec![(1.0, 0.0)];
        }
        let Some(path) = Path::new(from, to, arc, 0.0) else {
            return vec![(1.0, 0.0)];
        };
        let steps = (path.length / (CELL / 2.0)).ceil().max(1.0) as usize;
        let mut previous = self.across(&path, 0.0, reach);
        let mut spans = Vec::new();
        for step in 1..=steps {
            let share = step as f64 / steps as f64;
            let rise = self.across(&path, share, reach);
            let mut lower = (step - 1) as f64 / steps as f64;
            while rise != previous {
                let mut upper = share;
                for _ in 0..12 {
                    let middle = (lower + upper) / 2.0;
                    if self.across(&path, middle, reach) == previous {
                        lower = middle;
                    } else {
                        upper = middle;
                    }
                }
                spans.push(((lower + upper) / 2.0, previous));
                previous = self.across(&path, upper, reach);
                lower = upper;
            }
        }
        spans.push((1.0, previous));
        spans
    }

    pub(super) fn clear(&mut self) {
        self.paths.clear();
        self.cells.clear();
    }

    pub(super) fn add(&mut self, from: (f64, f64), to: (f64, f64), arc: Option<Arc>, rise: f64) {
        let Some(path) = Path::new(from, to, arc, rise) else {
            return;
        };
        let index = self.paths.len();
        let steps = (path.length / (CELL / 2.0)).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let point = path.at(step as f64 / steps as f64);
            let entries = self
                .cells
                .entry(Grid::default().at(point.0, point.1))
                .or_default();
            if entries.last() != Some(&index) {
                entries.push(index);
            }
        }
        self.paths.push(path);
    }

    pub(super) fn at(&self, point: (f64, f64), reach: f64) -> f64 {
        let cell = Grid::default().at(point.0, point.1);
        // `Grid::at` saturates a coordinate past the cell range, so both this
        // and the neighbour below it can overflow — and a wrapped cell is not
        // a missing cell, it is somebody else's rise handed back as this
        // bead's ground. Saturating is what `footprint::floor` does for the
        // other axis and what this module's own rule asks for: a coordinate no
        // printer could reach warns and carries on, it never fails.
        let neighbors = ((reach / CELL).ceil() as i32).saturating_add(1);
        let mut nearest = reach;
        let mut rise = 0.0_f64;
        for column in -neighbors..=neighbors {
            for row in -neighbors..=neighbors {
                let key = (cell.0.saturating_add(column), cell.1.saturating_add(row));
                if let Some(entries) = self.cells.get(&key) {
                    for &index in entries {
                        let path = &self.paths[index];
                        let distance = path.distance(point);
                        if distance < nearest || (distance == nearest && path.rise > rise) {
                            nearest = distance;
                            rise = path.rise;
                        }
                    }
                }
            }
        }
        rise
    }

    pub(super) fn mean(
        &self,
        from: (f64, f64),
        to: (f64, f64),
        arc: Option<Arc>,
        reach: f64,
    ) -> f64 {
        if self.paths.is_empty() {
            return 0.0;
        }
        let Some(path) = Path::new(from, to, arc, 0.0) else {
            return 0.0;
        };
        let steps = (path.length / (CELL / 2.0)).ceil().max(1.0) as usize;
        let first = self.across(&path, 0.5 / steps as f64, reach);
        let sum: f64 = (1..steps)
            .map(|step| self.across(&path, (step as f64 + 0.5) / steps as f64, reach) - first)
            .sum();
        first + sum / steps as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighboring_gap_changes_each_keep_their_height() {
        let mut ground = Ground::default();
        ground.add((-1.0, 0.0), (0.04, 0.0), None, 0.1);
        ground.add((0.04, 0.0), (0.08, 0.0), None, 0.05);
        ground.add((0.08, 0.0), (2.0, 0.0), None, 0.0);
        let mut previous = 0.0;
        let mut integral = 0.0;
        for (end, rise) in ground.profile((0.0, 0.0), (1.0, 0.0), None, 0.2) {
            integral += (end - previous) * rise;
            previous = end;
        }
        assert!((integral - 0.006).abs() < 0.00001, "{integral}");
    }

    #[test]
    fn a_gap_change_near_either_endpoint_is_not_skipped() {
        for boundary in [0.03, 0.97] {
            let mut ground = Ground::default();
            ground.add((-1.0, 0.0), (boundary, 0.0), None, 0.0);
            ground.add((boundary, 0.0), (2.0, 0.0), None, 0.1);
            let spans = ground.profile((0.0, 0.0), (1.0, 0.0), None, 0.2);
            let mut previous = 0.0;
            let mut integral = 0.0;
            for (end, rise) in spans {
                integral += (end - previous) * rise;
                previous = end;
            }
            assert!(
                (integral - (1.0 - boundary) * 0.1).abs() < 0.00001,
                "boundary {boundary}: {integral}"
            );
        }
    }
}
