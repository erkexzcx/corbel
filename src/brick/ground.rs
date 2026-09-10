use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f64::consts::TAU;

use crate::geometry::{Arc, CELL, Grid, Trace, footprint, turn};

/// How far apart the samples of one path are, in mm.
///
/// [`Ground::add`] lays them every [`CELL`] / 2 along the path, which is what
/// bounds how far a query point can be from a sample while still being near
/// the path — see [`Ground::at`].
const SAMPLE: f64 = CELL / 2.0;

#[derive(Default)]
pub(super) struct Ground {
    paths: Vec<Path>,
    cells: HashMap<(i32, i32), Vec<u32>>,
    /// One stamp per path, so a path whose samples land in several of the
    /// cells a query looks at is measured once for that query rather than
    /// once a cell. A wall's bead crosses two or three cells at [`CELL`], so
    /// without this the same path is measured again for every one of them.
    seen: RefCell<Vec<u64>>,
    /// Which stamp is current. Incremented per query, so a stale stamp cannot
    /// be mistaken for this query's.
    generation: Cell<u64>,
    /// The window the last query swept, kept so the samples of one bead that
    /// land in the same cell sweep it once between them. This is the whole of
    /// the module's speed: measured on a real 3.7 MB slice, `Ground::across`
    /// — the sweep and the distance measured against what it found — was 97%
    /// of a `--bricks` run, and the sweep was made again for every sample
    /// point that happened to land in the cell just swept.
    gathered: RefCell<Gathered>,
}

/// The paths a query standing in one grid cell may be measured against.
#[derive(Default)]
struct Gathered {
    cell: Option<(i32, i32)>,
    /// The reach the window was sized for, as bits — a query at a different
    /// reach wants a different window.
    reach: u64,
    paths: Vec<u32>,
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
        // The window a query swept is a list of indices into `paths`, so it
        // does not survive them being dropped and taken up again: the layer
        // that fills this one lays down its own beads, and the next query in
        // the cell the window was left holding would read whichever paths had
        // landed on those indices since — or past the end of the list.
        self.gathered.borrow_mut().cell = None;
    }

    pub(super) fn add(&mut self, from: (f64, f64), to: (f64, f64), arc: Option<Arc>, rise: f64) {
        let Some(path) = Path::new(from, to, arc, rise) else {
            return;
        };
        let index = self.paths.len() as u32;
        let steps = (path.length / SAMPLE).ceil().max(1.0) as usize;
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
        let mut gathered = self.gathered.borrow_mut();
        if gathered.cell != Some(cell) || gathered.reach != reach.to_bits() {
            self.gather(cell, reach, &mut gathered);
        }
        let mut nearest = reach;
        let mut rise = 0.0_f64;
        for &index in &gathered.paths {
            let path = &self.paths[index as usize];
            let distance = path.distance(point);
            if distance < nearest || (distance == nearest && path.rise > rise) {
                nearest = distance;
                rise = path.rise;
            }
        }
        rise
    }

    /// Collects the paths a query standing in `cell` may be measured against,
    /// each once, into `into`.
    ///
    /// `Grid::at` saturates a coordinate past the cell range, so both the cell
    /// and its neighbours can overflow — and a wrapped cell is not a missing
    /// cell, it is somebody else's rise handed back as this bead's ground.
    /// Saturating is what `footprint::floor` does for the other axis and what
    /// this module's own rule asks for: a coordinate no printer could reach
    /// warns and carries on, it never fails.
    ///
    /// How far the window reaches. A cell holds a path when one of its
    /// *samples* landed in it, so a path that is near the query point is not
    /// necessarily registered in a cell near it — what puts it in reach is
    /// where its nearest sample sits. That sample is within [`SAMPLE`] / 2 of
    /// the nearest point of the path along it, and a path that can win is
    /// within `reach` of the query, so its admitting sample is within
    /// `reach + SAMPLE / 2` of it. A point that close can only be one cell
    /// further along either axis at this grid, which is why the window is
    /// three cells square and not the five it used to be: the rings outside it
    /// can hold a path whose *nearest point* is within `reach` (the path loops
    /// back), but such a path has a sample in the middle window as well. This
    /// drops only paths whose every sample is past the reach, and those cannot
    /// win a comparison that starts at `reach` itself.
    fn gather(&self, cell: (i32, i32), reach: f64, into: &mut Gathered) {
        let across = (reach + SAMPLE / 2.0) / CELL;
        let neighbors = if across.is_finite() && across > 0.0 {
            (across.floor() as i32).saturating_add(1)
        } else {
            1
        };
        let mut seen = self.seen.borrow_mut();
        if seen.len() < self.paths.len() {
            seen.resize(self.paths.len(), 0);
        }
        // Zero is never a live stamp: the array is filled with it when it is
        // grown, so a wrap would read a grown entry as already seen.
        let generation = self.generation.get().wrapping_add(1).max(1);
        self.generation.set(generation);
        into.paths.clear();
        for column in -neighbors..=neighbors {
            for row in -neighbors..=neighbors {
                let key = (cell.0.saturating_add(column), cell.1.saturating_add(row));
                if let Some(entries) = self.cells.get(&key) {
                    for &index in entries {
                        if seen[index as usize] == generation {
                            continue;
                        }
                        seen[index as usize] = generation;
                        into.paths.push(index);
                    }
                }
            }
        }
        into.cell = Some(cell);
        into.reach = reach.to_bits();
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

    /// A path within reach of a query is found even when not one of its
    /// samples landed in the query's own cell.
    ///
    /// A cell holds a path when one of its *samples* lands in it, so what puts
    /// a path in reach is where its samples are — and the window a query
    /// sweeps is sized from that. The window used to be five cells square on
    /// the reasoning that a path registered a couple of cells out could still
    /// have its nearest point nearby; the sample spacing says one cell either
    /// way is enough, and this is the path that proves it: it runs a tenth of
    /// a cell short of the cell boundary, so a query just inside its
    /// neighbour reaches it while sharing no cell with it at all.
    #[test]
    fn a_path_reaches_a_query_standing_in_the_cell_beside_it() {
        let cell = CELL;
        let x = cell * 1.05;
        let mut ground = Ground::default();
        ground.add((x, -cell), (x, cell), None, 0.1);
        assert_eq!(
            Grid::default().at(cell * 0.95, 0.0),
            (0, 0),
            "the query stands in the cell before the path's own"
        );
        assert_eq!(ground.at((cell * 0.95, 0.0), cell / 2.0), 0.1);
        // And the same query still reads the plane where the reach does not
        // stretch: the path is a tenth of a cell past it.
        assert_eq!(ground.at((cell * 0.95, 0.0), cell / 10.0), 0.0);
    }

    /// The window a query sweeps is remembered between samples that land in
    /// one cell, so a query in another cell has to sweep its own.
    #[test]
    fn a_query_in_another_cell_is_measured_against_that_cell_s_own_paths() {
        let mut ground = Ground::default();
        ground.add((0.0, 0.0), (0.6, 0.0), None, 0.1);
        ground.add((0.6, 0.6), (1.2, 0.6), None, 0.2);
        assert_eq!(ground.at((0.3, 0.0), 0.15), 0.1);
        assert_eq!(ground.at((0.9, 0.6), 0.15), 0.2);
        // Back to the first cell, whose answer must not have been overwritten
        // by the sweep the second one made.
        assert_eq!(ground.at((0.3, 0.0), 0.15), 0.1);
    }

    /// A layer's paths are dropped and laid down again, and the window a query
    /// swept holds indices into them rather than the paths themselves. Keeping
    /// it across that would answer from beads that are gone — or index past
    /// the end of a shorter layer, which is a panic on a real file.
    #[test]
    fn a_cleared_ground_forgets_the_window_it_had_swept() {
        let mut ground = Ground::default();
        ground.add((0.0, 0.0), (0.6, 0.0), None, 0.1);
        ground.add((0.0, 0.3), (0.6, 0.3), None, 0.1);
        ground.add((0.0, 0.6), (0.6, 0.6), None, 0.1);
        assert_eq!(ground.at((0.3, 0.3), 0.15), 0.1);

        // One path where there were three, queried from the very cell the
        // window was left holding.
        ground.clear();
        ground.add((0.3, 0.0), (0.3, 0.6), None, 0.2);
        assert_eq!(ground.at((0.3, 0.3), 0.15), 0.2);
    }

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
