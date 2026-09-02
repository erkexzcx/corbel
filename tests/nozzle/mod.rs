//! A nozzle replayed over finished G-code, so that what a transform wrote can
//! be checked against what a printer would physically do with it.
//!
//! Three invariants, and between them they cover every way a post-processor
//! can wreck a print by moving the toolhead rather than by mis-metering it:
//!
//! 1. **the nozzle is never taken under the bed**;
//! 2. **the nozzle never drives into a crest** — material standing above it,
//!    inside its own footprint, whose height does not go on rising away from
//!    it;
//! 3. **no bead is dragged** — a move that extrudes while going far further
//!    than the filament it carries can cover.
//!
//! The second is the general form of "it blasts through the raised walls", and
//! the distinction it draws is the one a flat nozzle actually makes. A surface
//! that rises away from the nozzle is a slope, and printing slopes is what
//! printers do: the flat rides up it. A crest is different — the peak sits
//! inside the flat with lower material beyond it, so there is nothing holding
//! the nozzle off and it shears the peak away. A bead raised half a layer
//! beside two flat ones is a crest; a ramp is not.
//!
//! It is measured, not asserted: on the file the user reported, before the
//! fix, **3400 crests in 25 layers**; on the same file after it, **none**.
//!
//! The third is what a reordering pass gets wrong. Move a loop and its travel
//! has to move with it, or the loop is drawn from wherever the reorder left
//! the nozzle — a bead across the whole bed, metered for the millimetre it was
//! supposed to be. Heights alone cannot see that: it happens entirely on the
//! layer's own plane.
//!
//! Nothing here reads a marker to decide what a move *means*. It replays
//! coordinates, so a transform cannot satisfy it by relabelling its output.

#![allow(dead_code)]

use std::collections::HashMap;

use corbel::gcode::{Code, Extruder, Line, Modal};
use corbel::geometry::{Arc, turn};

/// How far apart, in mm of path, a move is sampled.
///
/// Fine enough that a bead 0.4 mm wide cannot be stepped over, coarse enough
/// that a real slice is replayed in a second.
const STEP: f64 = 0.1;

/// Heights closer than this are the same height. A bead laid at the plane the
/// nozzle is standing on is touched, not plowed, and that is the common case
/// on every layer of every file.
const EPSILON: f64 = 1e-6;

/// Samples held before the field is pruned whatever the file has said about
/// layers. Only the material within a layer height of the nozzle can ever be
/// reached, so this bounds the replay by geometry rather than by file size.
const CAPACITY: usize = 1 << 20;

/// The smallest footprint a real nozzle can be claimed to have, and the widest
/// bead the file says it lays.
#[derive(Clone, Copy, Debug)]
pub struct Nozzle {
    /// The orifice. Every nozzle has a flat around it wider than this, so
    /// taking the bore alone is the most generous reading available: anything
    /// this reports would be hit by any real hot end.
    pub bore: f64,
    pub bead: f64,
    pub layer: f64,
}

impl Default for Nozzle {
    fn default() -> Self {
        Self {
            bore: 0.4,
            bead: 0.45,
            layer: 0.2,
        }
    }
}

impl Nozzle {
    /// How far from the path centreline the nozzle can reach material: its own
    /// radius, plus half of the bead whose centreline is being measured to.
    pub fn reach(&self) -> f64 {
        self.bore / 2.0 + self.bead / 2.0
    }

    /// The geometry the file states about itself. Read here rather than
    /// through the crate's own settings code, so a defect in that cannot hide
    /// a collision from this.
    pub fn read(gcode: &str) -> Self {
        let mut nozzle = Self::default();
        for line in gcode
            .lines()
            .take_while(|line| !line.contains("EXECUTABLE"))
        {
            let Some((key, value)) = line.trim_start_matches(';').split_once('=') else {
                continue;
            };
            let key = key.trim();
            let first = value.trim().split(',').next().unwrap_or("").trim();
            let Ok(value) = first.parse::<f64>() else {
                continue;
            };
            if !value.is_finite() || value <= 0.0 {
                continue;
            }
            match key {
                "nozzle_diameter" => nozzle.bore = value,
                "layer_height" => nozzle.layer = value,
                "inner_wall_line_width" | "perimeter_extrusion_width" => nozzle.bead = value,
                _ => {}
            }
        }
        nozzle
    }
}

/// One place the nozzle drove into a crest that was already there.
#[derive(Clone, Debug)]
pub struct Plunge {
    pub line: usize,
    pub at: (f64, f64, f64),
    /// The top of the material it went through.
    pub top: f64,
    /// True where it was laying a bead at the time, false on a travel.
    pub extruding: bool,
    pub text: String,
}

impl Plunge {
    pub fn depth(&self) -> f64 {
        self.top - self.at.2
    }
}

/// A move that extrudes while going further than its filament can cover.
#[derive(Clone, Debug)]
pub struct Drag {
    pub line: usize,
    pub span: f64,
    /// Filament per mm of travel, against what the file's own beads run at.
    pub rate: f64,
    pub text: String,
}

/// How far below the file's own median a bead's filament per mm has to fall
/// before it is being dragged rather than laid.
///
/// A fiftieth, and it sits in a valley two measured populations wide. What a
/// slicer itself emits bottoms out at **0.135 of its own median** on the
/// 10-object plates — a `; LINE_WIDTH: 0.121649` thread against a 0.42 mm
/// wall — and bricking may halve that again where it caps a column, so a
/// correct output reaches **0.0723** and no lower. The bead that found this
/// carried 0.01 mm of filament over 155.9 mm: **0.0019 of the median**, ten
/// times under this bar where the thinnest honest bead is three times over it.
///
/// Do not raise it back toward a tenth. At 0.1 the two plates reported 2 and
/// 128 "drags" that were all the slicer's own thin threads.
const STARVED: f64 = 0.02;

/// The shortest move worth judging. A slicer ends a loop on a fraction of a
/// millimetre and rounds its filament to five decimals, so the rate on a very
/// short move is quantisation rather than a reading.
const LONG_ENOUGH: f64 = 2.0;

/// A height change written as a move of its own, made while the nozzle still
/// holds pressure.
///
/// A `G1` naming `Z` and nothing else gives the planner no direction to blend
/// into, so the toolhead comes to a dead stop to run it. Stopped and primed,
/// the nozzle oozes, and where it does that is the loop's own start point —
/// the seam. Measured on a real PETG part before this was fixed: 679 stops,
/// 13.5 s of standing still, and the stringing to show for it.
///
/// After a retraction the same move is harmless, which is why this is not
/// simply a count of `G1 Z` lines.
#[derive(Clone, Debug)]
pub struct Stop {
    pub line: usize,
    pub at: f64,
    pub text: String,
}

/// A bead laid above the plane its own layer sits on.
#[derive(Clone, Debug)]
pub struct Float {
    pub line: usize,
    pub layer: usize,
    pub at: f64,
    pub plane: f64,
    pub text: String,
}

impl Float {
    pub fn above(&self) -> f64 {
        self.at - self.plane
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub plunges: Vec<Plunge>,
    /// Lines that take the nozzle to a height no printer can reach.
    pub under_the_bed: Vec<(usize, f64, String)>,
    pub dragged: Vec<Drag>,
    /// The lowest bead of each layer, which is the plane that layer sits on,
    /// and the highest, which is what has to be judged against it.
    pub floors: Vec<f64>,
    pub ceilings: Vec<Option<Float>>,
    /// Height changes written as a move of their own while primed.
    pub stops: Vec<Stop>,
    /// How many moves pull filament back. A transform that reorders a wall has
    /// to carry each loop's wipe with it; dropping one leaves the nozzle
    /// primed through the travel that follows, which is where stringing comes
    /// from, and leaves the prime that answered it unbalanced.
    pub retractions: usize,
    pub moves: usize,
    pub beads: usize,
    pub nozzle: Nozzle,
}

impl Report {
    pub fn is_clear(&self) -> bool {
        self.plunges.is_empty() && self.under_the_bed.is_empty() && self.dragged.is_empty()
    }

    pub fn worst(&self) -> f64 {
        self.plunges
            .iter()
            .map(Plunge::depth)
            .fold(0.0, |worst, depth| worst.max(depth))
    }

    /// Plunges made while laying a bead — a wall put down beside one already
    /// standing proud, which is what wall order decides.
    pub fn while_extruding(&self) -> impl Iterator<Item = &Plunge> {
        self.plunges.iter().filter(|plunge| plunge.extruding)
    }

    /// Plunges made on a travel — the nozzle crossing a raised wall on its way
    /// somewhere, with nothing coming out of it.
    pub fn while_travelling(&self) -> impl Iterator<Item = &Plunge> {
        self.plunges.iter().filter(|plunge| !plunge.extruding)
    }

    #[track_caller]
    pub fn assert_clear(&self, what: &str) {
        if self.is_clear() {
            return;
        }
        panic!("{}", self.complaint(what));
    }

    fn complaint(&self, what: &str) -> String {
        let mut text = format!(
            "{what}: the nozzle does not survive its own output\n  \
             replayed {} moves and {} beads at a bore of {:.2} mm laying {:.2} mm beads\n",
            self.moves, self.beads, self.nozzle.bore, self.nozzle.bead
        );
        if !self.under_the_bed.is_empty() {
            text.push_str(&format!(
                "  under the bed: {} moves, lowest Z {:.3}\n",
                self.under_the_bed.len(),
                self.under_the_bed
                    .iter()
                    .map(|(_, z, _)| *z)
                    .fold(f64::INFINITY, f64::min)
            ));
            for (line, z, said) in self.under_the_bed.iter().take(3) {
                text.push_str(&format!("    line {line}: Z{z:.3}  {said}\n"));
            }
        }
        if !self.dragged.is_empty() {
            text.push_str(&format!(
                "  beads dragged: {} moves, longest {:.1} mm\n",
                self.dragged.len(),
                self.dragged
                    .iter()
                    .map(|drag| drag.span)
                    .fold(0.0_f64, f64::max)
            ));
            let mut worst: Vec<&Drag> = self.dragged.iter().collect();
            worst.sort_by(|left, right| right.span.total_cmp(&left.span));
            for drag in worst.into_iter().take(3) {
                text.push_str(&format!(
                    "    line {}: {:.1} mm at {:.4} of the filament its neighbours carry\n      {}\n",
                    drag.line,
                    drag.span,
                    drag.rate,
                    drag.text.trim()
                ));
            }
        }
        if !self.plunges.is_empty() {
            let extruding = self.while_extruding().count();
            text.push_str(&format!(
                "  through standing material: {} moves ({extruding} of them laying a bead, \
                 {} of them travelling), worst {:.0} um deep\n",
                self.plunges.len(),
                self.plunges.len() - extruding,
                self.worst() * 1000.0
            ));
            let mut worst: Vec<&Plunge> = self.plunges.iter().collect();
            worst.sort_by(|a, b| b.depth().total_cmp(&a.depth()));
            for plunge in worst.into_iter().take(4) {
                let doing = if plunge.extruding {
                    "laying a bead"
                } else {
                    "travelling"
                };
                text.push_str(&format!(
                    "    line {}: {doing} at Z{:.3} under material topping {:.3} \
                     ({:.0} um) at X{:.3} Y{:.3}\n      {}\n",
                    plunge.line,
                    plunge.at.2,
                    plunge.top,
                    plunge.depth() * 1000.0,
                    plunge.at.0,
                    plunge.at.1,
                    plunge.text.trim()
                ));
            }
        }
        text
    }
}

pub fn inspect(gcode: &str) -> Report {
    inspect_as(gcode, Nozzle::read(gcode))
}

pub fn inspect_as(gcode: &str, nozzle: Nozzle) -> Report {
    let mut field = Field::new(nozzle.reach());
    let mut modal = Modal::new();
    let mut extruder = Extruder::new();
    let mut report = Report {
        plunges: Vec::new(),
        under_the_bed: Vec::new(),
        dragged: Vec::new(),
        floors: Vec::new(),
        ceilings: Vec::new(),
        stops: Vec::new(),
        retractions: 0,
        moves: 0,
        beads: 0,
        nozzle,
    };
    // Every bead's filament per mm of travel, so the starved ones can be
    // judged against what this file's own beads run at rather than against a
    // figure picked here.
    let mut rates: Vec<(usize, f64, f64, String)> = Vec::new();
    // The bead being laid right now. The nozzle's own flat presses what it put
    // down a moment ago and is meant to, so a run is only part of the world
    // once something else has happened.
    let mut run: Vec<(f64, f64, f64)> = Vec::new();
    let mut path = Vec::new();
    let mut layer = 0usize;
    // How much filament has been pulled back and not yet put in. Stopping the
    // toolhead only oozes while this is zero.
    let mut withdrawn = 0.0_f64;

    for (index, text) in gcode.lines().enumerate() {
        let line = Line::parse(text);
        match line.code {
            Code::AbsoluteE | Code::RelativeE => extruder.set_mode(line.code),
            Code::SetPosition => {
                if let Some(e) = line.e {
                    extruder.observe_origin(e);
                }
            }
            _ => {}
        }
        if is_layer_marker(text) {
            field.commit(run.drain(..));
            field.prune(modal.position().2 - nozzle.layer * 2.0);
            layer += 1;
        }
        let from = modal.position();
        let delta = line.e.filter(|_| line.draws()).map(|e| extruder.observe(e));
        // A retraction and its prime name no coordinate; a bead's own filament
        // is not a prime and must not cancel one.
        if let Some(value) = delta.filter(|_| line.x.is_none() && line.y.is_none()) {
            withdrawn = (withdrawn - value).max(0.0);
        }
        if delta.is_some_and(|value| value < 0.0) {
            report.retractions += 1;
        }
        // A height change that names nothing else gives the planner nothing to
        // blend into, so the toolhead stops dead to run it.
        if line.draws()
            && line.z.is_some()
            && line.x.is_none()
            && line.y.is_none()
            && delta.is_none()
            && withdrawn <= EPSILON
        {
            report.stops.push(Stop {
                line: index + 1,
                at: modal.position().2,
                text: text.to_owned(),
            });
        }
        let Some(to) = modal.apply(&line) else {
            continue;
        };
        let arc = line.arc_between((from.0, from.1), (to.0, to.1));
        // A retraction, a prime or a bare feedrate is a `G1` that goes
        // nowhere. The nozzle is wherever the move before it left it, and
        // blaming a collision on a line that did not move is a diagnostic
        // nobody can act on.
        if arc.is_none() && to == from {
            continue;
        }
        report.moves += 1;
        let lays = line.draws_in_plane() && delta.is_some_and(|value| value > 0.0);
        report.beads += usize::from(lays);

        path.clear();
        walk(from, to, arc, &mut path);
        if lays {
            let span = span_of(&path);
            if span > LONG_ENOUGH {
                let carried = delta.unwrap_or(0.0);
                rates.push((index + 1, span, carried / span, text.to_owned()));
            }
        }

        let mut lowest = f64::INFINITY;
        let mut deepest: Option<Plunge> = None;
        for &(x, y, z) in path.iter() {
            lowest = lowest.min(z);
            if let Some(top) = field.crest(x, y, z)
                && deepest
                    .as_ref()
                    .is_none_or(|plunge| top - z > plunge.depth())
            {
                deepest = Some(Plunge {
                    line: index + 1,
                    at: (x, y, z),
                    top,
                    extruding: lays,
                    text: text.to_owned(),
                });
            }
        }
        if let Some(plunge) = deepest {
            report.plunges.push(plunge);
        }
        if lowest < -EPSILON || !lowest.is_finite() {
            report
                .under_the_bed
                .push((index + 1, lowest, text.to_owned()));
        }

        // Where this layer's beads sit. The lowest is the plane the layer was
        // sliced for; anything meaningfully above it was not printed onto the
        // layer below but into the space over it.
        if lays {
            while report.floors.len() <= layer {
                report.floors.push(f64::INFINITY);
                report.ceilings.push(None);
            }
            for &(_, _, z) in path.iter() {
                report.floors[layer] = report.floors[layer].min(z);
                if report.ceilings[layer].as_ref().is_none_or(|had| z > had.at) {
                    report.ceilings[layer] = Some(Float {
                        line: index + 1,
                        layer,
                        at: z,
                        plane: f64::NAN,
                        text: text.to_owned(),
                    });
                }
            }
        }

        if lays {
            run.extend(path.iter().copied());
        } else {
            field.commit(run.drain(..));
        }
        if field.held >= CAPACITY {
            field.prune(to.2 - nozzle.layer);
        }
    }

    // A bead metered for a millimetre and made to cross the bed carries a
    // filament rate hundreds of times under what its neighbours run at, so the
    // file's own median is the yardstick and nothing has to be assumed about
    // the profile.
    let mut sorted: Vec<f64> = rates.iter().map(|(_, _, rate, _)| *rate).collect();
    sorted.sort_by(f64::total_cmp);
    if let Some(&median) = sorted.get(sorted.len() / 2) {
        let floor = median * STARVED;
        report.dragged = rates
            .into_iter()
            .filter(|(_, _, rate, _)| *rate < floor)
            .map(|(line, span, rate, text)| Drag {
                line,
                span,
                rate: rate / median,
                text,
            })
            .collect();
    }
    report
}

/// How far a walked path runs in the plane.
fn span_of(path: &[(f64, f64, f64)]) -> f64 {
    path.windows(2)
        .map(|pair| (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1))
        .sum()
}

fn is_layer_marker(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with(";LAYER_CHANGE")
        || text.starts_with("; CHANGE_LAYER")
        || text.starts_with(";LAYER:")
        || text.starts_with(";AFTER_LAYER_CHANGE")
}

/// Every point the nozzle passes through on one move, at the height it is at
/// when it passes through it.
fn walk(
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    arc: Option<Arc>,
    out: &mut Vec<(f64, f64, f64)>,
) {
    let turned = arc.and_then(|arc| turn((from.0, from.1), (to.0, to.1), arc));
    if let Some((centre, radius, start, sweep)) = turned {
        let steps = ((sweep.abs() * radius / STEP).ceil() as usize).clamp(2, 4096);
        for step in 0..=steps {
            let share = step as f64 / steps as f64;
            let angle = start + sweep * share;
            out.push((
                centre.0 + radius * angle.cos(),
                centre.1 + radius * angle.sin(),
                from.2 + (to.2 - from.2) * share,
            ));
        }
        return;
    }
    let span = (to.0 - from.0).hypot(to.1 - from.1);
    let steps = ((span / STEP).ceil() as usize).clamp(1, 4096);
    for step in 0..=steps {
        let share = step as f64 / steps as f64;
        out.push((
            from.0 + (to.0 - from.0) * share,
            from.1 + (to.1 - from.1) * share,
            from.2 + (to.2 - from.2) * share,
        ));
    }
}

/// Where material stands, as the tops of the bead centrelines that laid it.
///
/// Bucketed at the nozzle's own reach, so the material that can be touched
/// from a point is always in the nine buckets around it. Each bucket carries
/// the highest thing in it and is held in descending order, which is what
/// makes this cheap: on a normal layer everything is at the plane the nozzle
/// stands on, so a bucket is dismissed on one comparison, and a bucket that
/// does hold something proud is walked only as far as the proud part of it.
struct Field {
    reach: f64,
    cells: HashMap<(i32, i32), Bucket>,
    touched: Vec<(i32, i32)>,
    held: usize,
}

#[derive(Default)]
struct Bucket {
    top: f64,
    points: Vec<(f64, f64, f64)>,
}

impl Field {
    fn new(reach: f64) -> Self {
        Self {
            reach,
            cells: HashMap::new(),
            touched: Vec::new(),
            held: 0,
        }
    }

    fn key(&self, x: f64, y: f64) -> (i32, i32) {
        (
            (x / self.reach).floor() as i32,
            (y / self.reach).floor() as i32,
        )
    }

    fn commit(&mut self, points: impl Iterator<Item = (f64, f64, f64)>) {
        self.touched.clear();
        for (x, y, z) in points {
            let key = self.key(x, y);
            let bucket = self.cells.entry(key).or_insert_with(|| Bucket {
                top: f64::NEG_INFINITY,
                points: Vec::new(),
            });
            bucket.top = bucket.top.max(z);
            bucket.points.push((x, y, z));
            self.touched.push(key);
            self.held += 1;
        }
        self.touched.sort_unstable();
        self.touched.dedup();
        for key in &self.touched {
            if let Some(bucket) = self.cells.get_mut(key) {
                bucket
                    .points
                    .sort_unstable_by(|left, right| right.2.total_cmp(&left.2));
            }
        }
    }

    /// The top of a crest this point sits under, or `None` where there is no
    /// material above it or where the material is a slope rising away.
    ///
    /// A slope keeps climbing past the nozzle's own footprint; a crest does
    /// not. So the highest thing within one footprint is compared with the
    /// highest within two: if the wider look finds nothing taller, the peak is
    /// inside the flat and the nozzle has to shear it off.
    fn crest(&self, x: f64, y: f64, z: f64) -> Option<f64> {
        let (gx, gy) = self.key(x, y);
        let (mut near, mut far) = (None::<f64>, None::<f64>);
        for dx in -2..=2 {
            for dy in -2..=2 {
                let Some(bucket) = self.cells.get(&(gx + dx, gy + dy)) else {
                    continue;
                };
                if bucket.top <= z + EPSILON {
                    continue;
                }
                for &(bx, by, top) in &bucket.points {
                    if top <= z + EPSILON {
                        break;
                    }
                    let apart = (bx - x).hypot(by - y);
                    if apart <= self.reach {
                        near = Some(near.map_or(top, |had: f64| had.max(top)));
                    }
                    if apart <= self.reach * 2.0 {
                        far = Some(far.map_or(top, |had: f64| had.max(top)));
                    }
                }
            }
        }
        let near = near?;
        match far {
            // The surface goes on rising, so this is a slope being ridden.
            Some(far) if far > near + EPSILON => None,
            _ => Some(near),
        }
    }

    /// Forgets material below `floor`. The nozzle only ever descends within a
    /// layer by half of one, so nothing a layer below it can be reached again.
    fn prune(&mut self, floor: f64) {
        self.held = 0;
        self.cells.retain(|_, bucket| {
            bucket.points.retain(|(_, _, top)| *top >= floor);
            bucket.top = bucket
                .points
                .iter()
                .map(|(_, _, top)| *top)
                .fold(f64::NEG_INFINITY, f64::max);
            self.held += bucket.points.len();
            !bucket.points.is_empty()
        });
    }
}

/// What a transform has to hand back, whatever it does in between.
///
/// Everything here is read off the file knowing nothing about the transform,
/// so a change that breaks one of these cannot be hidden by relabelling. They
/// are compared between the input and the output rather than judged against a
/// figure picked here, which is what makes them survive a redesign.
pub struct Ledger {
    /// Extruding path length per layer. A loop lost, duplicated or dragged
    /// across the bed all move this and nothing else does.
    pub drawn: Vec<f64>,
    /// Every command that is not a move, in the order it was written. A
    /// retraction's `M`-code neighbours, a tool change, an origin reset.
    pub commands: Vec<String>,
    pub extruded: f64,
    pub retracted: f64,
    /// The slowest and fastest feedrate in force while laying a bead.
    pub bead_feed: (f64, f64),
    /// The fastest each tool's filament is asked to melt, in mm of it a
    /// second.
    ///
    /// A raise gives a bead more filament and the slicer's own rate stays
    /// where it was, so the hot end is asked for that much more melt — which
    /// it cannot deliver, so the one bead that has to carry a layer and a half
    /// comes out starved. Per TOOL: a plate printing two materials pins each
    /// to its own limit, and they can be 3x apart. Only beads long enough to
    /// measure: a coordinate is written to the micron, so a bead a few microns
    /// long divides one rounding by another.
    pub peak_melt: Vec<f64>,
    /// The line each peak was measured on, so a failure names it.
    pub peak_line: Vec<String>,
    /// Seconds spent laying bead. The rate a bead is laid at may be brought
    /// DOWN on purpose, to give the extra filament a raise adds the time it
    /// needs, so what has to be bounded is how much of that is spent — not
    /// whether any single rate changed.
    pub seconds: f64,
    /// What the file says each filament slot melts at, in mm of it a second.
    /// Read here rather than through the crate's own settings code, so a
    /// defect in that cannot hide an over-fed bead from this.
    pub melt_ceiling: Vec<f64>,
    /// Moves that wind an absolute extruder backwards without retracting.
    pub backwards: Vec<(usize, String)>,
    /// How much bead is drawn at each width the slicer declared. Reordering a
    /// region's loops moves them away from the `; LINE_WIDTH:` that was stated
    /// once for all of them. Length, not moves: the surface transform splits
    /// one move into many and draws exactly the same line.
    pub widths: HashMap<String, f64>,
    /// How far the toolhead travels while the nozzle still holds pressure.
    ///
    /// A slicer leaves short hops between two loops of one wall unretracted,
    /// because a few millimetres primed cost nothing. Move one of those loops
    /// somewhere else and the same unretracted hop becomes a journey across
    /// the plate, which is where stringing comes from. The retraction COUNT is
    /// untouched by that, which is why counting them cannot see it.
    pub primed_travel: f64,
    /// Height changes written as a move of their own while primed, which stop
    /// the toolhead dead with a full nozzle over the seam.
    pub primed_stops: usize,
    /// How much bead is laid under each `; FEATURE:` the slicer declared.
    ///
    /// A slicer states the region once and then sets the fan, the speed and
    /// the acceleration for it. Writing a wall's loops after some other
    /// region without re-stating theirs prints them at that region's settings
    /// — measured on a real tree-support print, 5136 wall beads came out under
    /// `; FEATURE: Support`, which is 20% fan on a wall.
    pub regions: HashMap<String, f64>,
    /// Beads that stand above both their neighbours more steeply than the
    /// nozzle could climb to them and back off.
    ///
    /// A flat nozzle cannot rear up and come straight back within its own
    /// underside; asked to, it smears the crest and the pass beside it comes
    /// down on whatever is left. In a preview they are dots. One in one is far
    /// looser than anything a surface really does — a real ramp measures under
    /// a fifth of that — so this only ever catches a jab.
    pub jabs: usize,
    /// Height moves that drop the nozzle and are then travelled away from
    /// before anything is drawn.
    ///
    /// A slicer lifts before a long travel and comes down on arrival. Putting
    /// the nozzle down in front of that travel makes the journey at bead
    /// height instead, over whatever the layer has already laid.
    pub dropped_hops: usize,
    /// Every place a support region lays a bead, in the order it lays them.
    ///
    /// Support is not the part. It is printed to be broken off, it is a single
    /// bead wide, and a tree support is tall and unbraced — so nothing this
    /// tool does may reach it. It has no wall to raise, no surface to follow
    /// and nothing above it that a step could meet, which makes "identical" a
    /// bound that can be asserted outright rather than a budget.
    pub supports: Vec<(i64, i64, i64)>,
}

pub fn ledger(gcode: &str) -> Ledger {
    let mut modal = Modal::new();
    let mut extruder = Extruder::new();
    let mut book = Ledger {
        drawn: Vec::new(),
        commands: Vec::new(),
        extruded: 0.0,
        retracted: 0.0,
        bead_feed: (f64::INFINITY, 0.0),
        peak_melt: Vec::new(),
        peak_line: Vec::new(),
        melt_ceiling: stated_melt(gcode),
        seconds: 0.0,
        backwards: Vec::new(),
        widths: HashMap::new(),
        primed_travel: 0.0,
        primed_stops: 0,
        regions: HashMap::new(),
        jabs: 0,
        dropped_hops: 0,
        supports: Vec::new(),
    };
    // Filament pulled back and not yet put in. Only a nozzle at zero oozes.
    let mut withdrawn = 0.0_f64;
    let mut standing = false;
    let mut supporting = false;
    let mut dropped: Option<f64> = None;
    let mut crest: Vec<(f64, f64)> = Vec::new();
    let mut region = String::new();
    let mut width = String::new();
    let mut layer = 0usize;
    let mut feed = 0.0_f64;
    let mut tool = 0usize;

    for (index, text) in gcode.lines().enumerate() {
        let line = Line::parse(text);
        match line.code {
            Code::AbsoluteE | Code::RelativeE => extruder.set_mode(line.code),
            Code::SetPosition => {
                if let Some(e) = line.e {
                    extruder.observe_origin(e);
                }
            }
            _ => {}
        }
        if is_layer_marker(text) {
            layer += 1;
        }
        let trimmed = text.trim();
        if let Some(digits) = trimmed.split_whitespace().next().and_then(|word| {
            word.strip_prefix('T')
                .and_then(|rest| rest.parse::<usize>().ok())
        }) && digits < 64
        {
            tool = digits;
        }
        if let Some(rest) = trimmed
            .strip_prefix("; FEATURE:")
            .or_else(|| trimmed.strip_prefix(";TYPE:"))
        {
            region = rest.trim().to_ascii_lowercase();
            supporting = region.contains("support");
        }
        if let Some(rest) = trimmed.strip_prefix("; LINE_WIDTH:") {
            width = rest.trim().to_owned();
        }
        // A command that moves nothing still sets the machine up for what
        // does, so losing one or reordering it changes the print.
        if !trimmed.is_empty()
            && !trimmed.starts_with(';')
            && !line.draws()
            && line.code != Code::AbsoluteE
            && line.code != Code::RelativeE
        {
            book.commands.push(
                trimmed
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_uppercase(),
            );
        }
        if let Some(rate) = line.f {
            feed = rate;
        }
        let from = modal.position();
        let delta = line.e.filter(|_| line.draws()).map(|e| extruder.observe(e));
        // A retraction and its prime name no coordinate; a bead's own
        // filament is not a prime and must not cancel one, but a bead does
        // prove the nozzle is full.
        // A wipe pulls filament back along a path, so most of a retraction is
        // named on a line that also names a coordinate. Counting only the bare
        // ones reads a wiped nozzle as a full one.
        if let Some(value) = delta {
            withdrawn = (withdrawn - value).max(0.0);
        }
        // A height written as a move of its own stops the toolhead dead.
        // What escapes while it stands there only matters if the nozzle then
        // goes somewhere: a stop that draws again on the spot oozes into its
        // own bead. So the stop is only counted once a travel follows it.
        if line.draws() && (line.x.is_some() || line.y.is_some()) {
            if standing && delta.is_none_or(|value| value <= 0.0) {
                book.primed_stops += 1;
            }
            standing = false;
        }
        if line.draws()
            && line.z.is_some()
            && line.x.is_none()
            && line.y.is_none()
            && delta.is_none()
            && withdrawn <= 1e-9
        {
            standing = true;
        }
        if let Some(value) = delta {
            if value > 0.0 {
                book.extruded += value;
            } else {
                book.retracted -= value;
                if !extruder.is_absolute() {
                    // Nothing to check: a relative retraction is a negative
                    // word and says so.
                } else if line.x.is_some() || line.y.is_some() {
                    book.backwards.push((index + 1, text.to_owned()));
                }
            }
        }
        let Some(to) = modal.apply(&line) else {
            continue;
        };
        // Material, not travel: where the nozzle passes over a support is
        // the slicer's business, but where it lays a bead is the support.
        if line.draws() {
            let fell = line.z.is_some_and(|z| z < from.2 - 1e-9);
            let steers = line.x.is_some() || line.y.is_some();
            let draws = delta.is_some_and(|value| value > 0.0);
            match (fell, steers, draws) {
                (true, false, false) => dropped = Some(from.2),
                (_, true, false) if dropped.is_some() => {
                    book.dropped_hops += 1;
                    dropped = None;
                }
                (_, _, true) => dropped = None,
                _ => {}
            }
        }
        // Only along an unbroken run of bead. A height change either side of
        // a travel is the nozzle lifting and coming back down, which is not a
        // crest and leaves nothing behind.
        if !(line.draws_in_plane() && delta.is_some_and(|value| value > 0.0)) {
            crest.clear();
        }
        if line.draws_in_plane() && delta.is_some_and(|value| value > 0.0) {
            let run = (to.0 - from.0).hypot(to.1 - from.1);
            crest.push((to.2, run));
            if crest.len() == 3 {
                let (low, _) = crest[0];
                let (top, up) = crest[1];
                let (next, down) = crest[2];
                let steep = |rise: f64, run: f64| rise > 0.0 && rise > run;
                if steep(top - low, up) && steep(top - next, down) {
                    book.jabs += 1;
                }
                crest.remove(0);
            }
        }
        if supporting
            && (line.x.is_some() || line.y.is_some())
            && delta.is_some_and(|value| value > 0.0)
        {
            // A micron is finer than any printer resolves and far finer than
            // the offset the visible wall is moved by, so this is exact
            // without asking two f64 to be bit-identical.
            book.supports.push((
                (to.0 * 1000.0).round() as i64,
                (to.1 * 1000.0).round() as i64,
                (to.2 * 1000.0).round() as i64,
            ));
        }
        if line.draws_in_plane() && delta.is_some_and(|value| value > 0.0) {
            while book.drawn.len() <= layer {
                book.drawn.push(0.0);
            }
            let along = match line.arc_between((from.0, from.1), (to.0, to.1)) {
                Some(arc) => swept((from.0, from.1), (to.0, to.1), arc),
                None => (to.0 - from.0).hypot(to.1 - from.1),
            };
            book.drawn[layer] += along;
            *book.widths.entry(width.clone()).or_default() += along;
            *book.regions.entry(region.clone()).or_default() += along;
            if feed > 0.0 {
                book.bead_feed.0 = book.bead_feed.0.min(feed);
                book.bead_feed.1 = book.bead_feed.1.max(feed);
                book.seconds += along / (feed / 60.0);
                if along >= MELT_GAUGE
                    && let Some(value) = delta.filter(|value| *value > 0.0)
                {
                    let melt = value / along * feed / 60.0;
                    if book.peak_melt.len() <= tool {
                        book.peak_melt.resize(tool + 1, 0.0);
                        book.peak_line.resize(tool + 1, String::new());
                    }
                    if melt > book.peak_melt[tool] {
                        book.peak_melt[tool] = melt;
                        book.peak_line[tool] = format!("{}: {}", index + 1, text.trim());
                    }
                }
            }
        } else if (line.x.is_some() || line.y.is_some()) && withdrawn <= 1e-9 {
            book.primed_travel += (to.0 - from.0).hypot(to.1 - from.1);
        }
    }
    book
}

/// How far round an arc actually goes. A move naming only a centre closes the
/// whole circle, which is why a zero sweep is a full turn and not nothing.
fn swept(from: (f64, f64), to: (f64, f64), arc: Arc) -> f64 {
    let centre = (from.0 + arc.i, from.1 + arc.j);
    let radius = arc.i.hypot(arc.j);
    let start = (from.1 - centre.1).atan2(from.0 - centre.0);
    let end = (to.1 - centre.1).atan2(to.0 - centre.0);
    let mut sweep = if arc.clockwise {
        start - end
    } else {
        end - start
    };
    if sweep <= 1e-12 {
        sweep += std::f64::consts::TAU;
    }
    radius * sweep
}

/// How much the visible wall's inward offset may move a layer's path length.
///
/// Moving a loop inward shortens it by `2 * PI * offset`, and the offset is a
/// fraction of a bead. Measured across the stored plates, no layer moves by
/// more than a few tenths of a percent; a loop lost or laid twice moves it by
/// whole percent.
const PATH_DRIFT: f64 = 0.02;

/// How much longer the beads of a file may take to lay.
///
/// A rate is brought down on purpose where a raise would otherwise ask the
/// hot end for melt it cannot deliver, and the deepest that can go is the
/// climb's own 1.54 — but only on the loops that climb, so across a file it
/// is a few percent. The bug this exists to catch put 44% of a plate at
/// F1800 against the F11054 it asked for, which is that bead running six
/// times as long.
const TIME_SLACK: f64 = 0.25;

/// The deepest a bead's rate is ever divided by. Slowing one to stay inside
/// the file's own melt rate divides its rate by the filament it was given,
/// and the most any bead is given is the climb's 1.54 — with a little room
/// for the flow multiplier on top of it.
const MOST_SLOWED: f64 = 1.65;

/// Shortest bead whose filament-per-mm is worth reading, in mm. A coordinate
/// is written to the micron, so anything shorter divides one rounding by
/// another.
const MELT_GAUGE: f64 = 0.5;

/// How far the filament a run actually adds may sit from the figure it prints.
///
/// The printed figure is the wall FLOW alone. A column also ramps on at its
/// first bead and is metered short where it is capped, and those two only
/// cancel for a column that both begins and ends inside the part — so a plate
/// full of short columns legitimately gains a little more than the flow
/// accounts for. Measured across the stored plates: 0.25% on the stock
/// profile, 0.66% at six walls, 0.84% at the deepest layer. The bug this
/// exists to catch was **13 percentage points** out, so there is a wide empty
/// valley between the two and this sits in it.
const CLAIM_SLACK: f64 = 1.0;

/// How much further than the input the nozzle may travel primed.
///
/// Nothing, to within rounding. The transform reorders loops; it has no reason
/// to move the toolhead further with a full nozzle than the slicer already
/// did, and every millimetre it adds is a millimetre of string.
const PRIMED_SLACK: f64 = 0.01;
/// Everything one file must still be able to say about another that came out
/// of the transform, gathered in one place so every suite asks the same
/// questions of every run.
///
/// `said` is what the binary printed about its own work. Where it is given,
/// the filament it claims to have added is checked against the filament it
/// actually added — a transform cannot be wrong in the same direction twice,
/// so agreeing with its own account is a stronger test than any bound picked
/// here.
/// The rate the file says its filament melts at, in mm of filament a second:
/// the slowest slot's `filament_max_volumetric_speed` over the filament's own
/// section. A print may use every bit of that, whether or not the slicer
/// happened to.
fn stated_melt(gcode: &str) -> Vec<f64> {
    let (mut rates, mut across) = (Vec::new(), 1.75);
    for line in gcode
        .lines()
        .take_while(|line| !line.contains("EXECUTABLE"))
    {
        let Some((key, value)) = line.trim_start_matches(';').split_once('=') else {
            continue;
        };
        match key.trim() {
            "filament_max_volumetric_speed" => {
                rates = value
                    .split(',')
                    .map(|piece| match piece.trim().parse::<f64>() {
                        Ok(slot) if slot > 0.0 && slot < 1000.0 => slot,
                        _ => 0.0,
                    })
                    .collect();
            }
            "filament_diameter" => {
                if let Ok(value) = value.split(',').next().unwrap_or("").trim().parse::<f64>()
                    && value > 0.5
                    && value < 5.0
                {
                    across = value;
                }
            }
            _ => {}
        }
    }
    let area = std::f64::consts::PI * (across / 2.0).powi(2);
    rates.iter().map(|rate| rate / area).collect()
}

pub fn faults(before: &Ledger, after: &Ledger, said: Option<&str>) -> Vec<String> {
    let mut found = Vec::new();

    for (layer, &was) in before.drawn.iter().enumerate() {
        let now = after.drawn.get(layer).copied().unwrap_or(0.0);
        if was > 1.0 && ((now - was) / was).abs() > PATH_DRIFT {
            found.push(format!(
                "layer {layer} draws {now:.1} mm against {was:.1} mm in the input ({:+.2}%) \
                 — a loop is lost, laid twice, or dragged",
                (now / was - 1.0) * 100.0
            ));
        }
    }

    if after.retracted + 1e-6 < before.retracted {
        found.push(format!(
            "{:.1} mm of retraction against {:.1} mm in the input — a wipe was dropped, \
             so the nozzle travels primed and the prime that answered it is unbalanced",
            after.retracted, before.retracted
        ));
    }

    if let Some((what, was, now)) = lost(&before.commands, &after.commands) {
        found.push(format!(
            "{was} of the input's {what} are {now} in the output — \
             {} commands in, {} out",
            before.commands.len(),
            after.commands.len()
        ));
    }

    // One-sided: a rate is brought DOWN on purpose where a raise would
    // otherwise ask the hot end for melt it cannot deliver, and `seconds`
    // bounds how much of that is spent. Faster than the slicer asked is
    // never anything but a bug.
    if after.bead_feed.1 > before.bead_feed.1 * 1.001 {
        found.push(format!(
            "a bead is laid at {:.0} mm/min, against {:.0} at the fastest in the input \
             — a feedrate was lost, so a bead runs at a travel's speed",
            after.bead_feed.1, before.bead_feed.1
        ));
    }

    // The other side has a floor, so it is checkable too. Slowing a bead to
    // stay inside the file's own melt rate divides its rate by the filament
    // it was given, and the most any bead is ever given is the climb's 1.54 —
    // so nothing may come out below that. Deeper than that is a rate that
    // belongs to something else: a height move at F600 under a wall printed
    // at F18000 is 30x, which reads as a bead at a thirtieth of the flow and
    // draws a line across the part.
    if after.bead_feed.0 < before.bead_feed.0 / MOST_SLOWED {
        found.push(format!(
            "a bead is laid at {:.0} mm/min, against {:.0} at the slowest in the input \
             — no metering divides a rate by more than {MOST_SLOWED}, so that rate \
             belongs to a height move or a retraction",
            after.bead_feed.0, before.bead_feed.0
        ));
    }

    if after.backwards.len() > before.backwards.len() {
        found.push(format!(
            "{} moves wind an absolute extruder backwards while laying a bead, against {} \
             in the input\n  {}",
            after.backwards.len(),
            before.backwards.len(),
            after.backwards[0].1.trim()
        ));
    }

    let astray: f64 = before
        .widths
        .iter()
        .map(|(width, was)| (was - after.widths.get(width).copied().unwrap_or(0.0)).abs())
        .sum::<f64>()
        / 2.0;
    let drawn: f64 = before.widths.values().sum();
    if drawn > 1.0 && astray / drawn > PATH_DRIFT {
        found.push(format!(
            "{astray:.0} mm of {drawn:.0} mm is drawn at a width the slicer did not declare \
             for it ({:.1}%) — a region states its `; LINE_WIDTH:` once and its loops were \
             reordered away from it",
            astray / drawn * 100.0
        ));
    }

    // The same shape, for the same reason, one axis over. A slicer states the
    // rate once and every bead behind it inherits one; a retraction or a
    // height move written between the two hands the wall the retraction's own
    // rate instead. Measured on a stock 1000-wall plate, 39446 mm of bead —
    // 44% of the file — came out at F1800 where the slicer asked for F11054,
    // with the bead's own `E`, the filament total and the feedrate RANGE all
    // untouched, because F1800 is a rate the file does use.
    //
    // Not the rates themselves: a rate is brought down ON PURPOSE where a
    // raise would otherwise ask the hot end for melt it cannot deliver, so
    // what is bounded is the TIME that costs. F1800 against F11054 over 44% of
    // a file is a wall running six times as long; a throttle is a few percent.
    if before.seconds > 1.0 && after.seconds > before.seconds * (1.0 + TIME_SLACK) {
        found.push(format!(
            "the beads take {:.0} s to lay against {:.0} s in the input ({:+.1}%) — `F` is \
             modal, so a bead behind an inserted retraction or height move is drawn at that \
             line's rate",
            after.seconds,
            before.seconds,
            (after.seconds / before.seconds - 1.0) * 100.0
        ));
    }

    // A raise gives a bead half a layer more filament and leaves the slicer's
    // rate where it was, so the hot end is asked for that much more melt per
    // second. Measured on a stock Bambu plate whose filament states 15 mm³/s
    // and whose input sits at exactly that for 98.64% of its path, `--bricks`
    // asked for up to 23 mm³/s — 54% over, across 16% of the file — and no hot
    // end delivers that, so the one bead that must carry a layer and a half to
    // fill the gap under a raised column is the one that comes out starved.
    //
    // Against the file's own stated ceiling as well as its fastest bead: a
    // profile may allow more melt than the slicer happened to use, and a print
    // with that headroom must not be slowed for nothing. Per TOOL, because a
    // plate printing two materials pins each to its own limit — measured on a
    // user's dual-nozzle plate, T0 peaks at exactly 8.00 mm³/s and T3 at
    // exactly 25.00, which is 3.1x apart.
    for (tool, &now) in after.peak_melt.iter().enumerate() {
        let was = before.peak_melt.get(tool).copied().unwrap_or_default();
        let stated = before.melt_ceiling.get(tool).copied().unwrap_or_default();
        let allowed = was.max(stated);
        if now > allowed * 1.001 {
            found.push(format!(
                "a bead is asked to melt filament {:.1}% faster than T{tool}'s filament \
                 allows — the hot end cannot deliver it, so the bead a raise depends on \
                 comes out starved\n    {}\n    against a ceiling of {allowed:.4} mm/s",
                (now / allowed.max(f64::MIN_POSITIVE) - 1.0) * 100.0,
                after.peak_line.get(tool).map_or("", String::as_str),
            ));
        }
    }

    // Stringing has one cause: the nozzle moving, or standing still, with
    // pressure behind it. A slicer decides where that is cheap — a hop between
    // two loops of one wall — and moving a loop somewhere else turns its cheap
    // hop into a journey. Neither the retraction count nor the filament total
    // changes when that happens, which is why both had to be joined by this.
    if after.primed_travel > before.primed_travel * (1.0 + PRIMED_SLACK) {
        found.push(format!(
            "the nozzle travels {:.0} mm primed against {:.0} mm in the input ({:+.0}%) \
             — a loop was moved away from the hop the slicer left unretracted, and it \
             strings the whole way",
            after.primed_travel,
            before.primed_travel,
            (after.primed_travel / before.primed_travel.max(1.0) - 1.0) * 100.0
        ));
    }
    for (region, was) in &before.regions {
        let now = after.regions.get(region).copied().unwrap_or_default();
        // A tenth is far wider than the visible wall's own inward offset moves
        // any region's length, and far narrower than a region gaining another
        // region's loops.
        if (now - was).abs() > was * 0.1 {
            found.push(format!(
                "{:.0} mm of bead is laid under `{region}` against {was:.0} mm in the \
                 input — a region states the fan, the speed and the acceleration \
                 once, so a loop written under someone else's marker prints at \
                 their settings",
                now
            ));
        }
    }
    if after.jabs > before.jabs {
        found.push(format!(
            "{} beads stand above both their neighbours more steeply than one in one, \
             against {} in the input — a flat nozzle cannot rear up and come straight \
             back within its own underside, and in a preview those are dots",
            after.jabs, before.jabs
        ));
    }
    if after.dropped_hops > before.dropped_hops {
        found.push(format!(
            "{} height moves put the nozzle down and are then travelled away from, against \
             {} in the input — a slicer lifts before a long travel and comes down on arrival, \
             so dropping it first makes that journey at bead height",
            after.dropped_hops, before.dropped_hops
        ));
    }
    if after.supports != before.supports {
        let moved = before
            .supports
            .iter()
            .zip(&after.supports)
            .filter(|(one, two)| one != two)
            .count();
        found.push(format!(
            "the support is not where the slicer put it: {} of its {} moves \
             changed and {} were added or lost — support is printed to be \
             broken off, so nothing here may reach it",
            moved,
            before.supports.len(),
            after.supports.len().abs_diff(before.supports.len())
        ));
    }
    if after.primed_stops > before.primed_stops {
        found.push(format!(
            "{} height changes are written as a move of their own while the nozzle is \
             primed, against {} in the input — each one stops the toolhead dead over a seam",
            after.primed_stops, before.primed_stops
        ));
    }

    if let Some(claim) = said.and_then(claimed) {
        // NET, not what was pushed out. A retraction dropped along the way
        // leaves the primes that answered it in place, so the part gains
        // filament without a single bead being metered differently — measured
        // on a real plate, +12.46% while every bead's own `E` was untouched.
        let (was, now) = (
            before.extruded - before.retracted,
            after.extruded - after.retracted,
        );
        let real = (now / was - 1.0) * 100.0;
        // A raise is charged where a column starts and given back where it
        // ends, so over a whole part the geometry nets out and only the flow
        // is left. A fixture is a WINDOW, and its top layer's raises are never
        // landed on: that half-layer of material really is in what was
        // written, and nothing later in the file pays it back. So the part may
        // run over the claim by up to the last layer's own share of it, and
        // may not fall short by anything — falling short is starvation, which
        // is what a threshold on that geometry used to cause.
        let standing = match before.drawn.iter().sum::<f64>() {
            total if total > 0.0 => 50.0 * before.drawn.last().copied().unwrap_or_default() / total,
            _ => 0.0,
        };
        if claim - real > CLAIM_SLACK || real - claim > CLAIM_SLACK + standing {
            found.push(format!(
                "it says it added {claim:+.2}% of filament and the part gained {real:+.2}%, \
                 outside the {:+.2} the last layer's own raises could account for",
                CLAIM_SLACK + standing
            ));
        }
    }
    found
}

/// The percentage the binary says its flow adds to the part.
fn claimed(said: &str) -> Option<f64> {
    let (_, rest) = said.split_once("adds ")?;
    let (figure, _) = rest.split_once('%')?;
    figure.trim().parse().ok()
}

/// The first command the output has fewer of than the input.
///
/// Counted, not ordered: a loop carries its own `M204` with it, so reordering
/// the loops of a wall reorders those too and that is the transform doing its
/// job. Losing one is not.
fn lost(before: &[String], after: &[String]) -> Option<(String, usize, usize)> {
    let mut want: HashMap<&str, usize> = HashMap::new();
    for command in before {
        *want.entry(command.as_str()).or_default() += 1;
    }
    let mut have: HashMap<&str, usize> = HashMap::new();
    for command in after {
        *have.entry(command.as_str()).or_default() += 1;
    }
    want.into_iter()
        .map(|(what, was)| (what, was, have.get(what).copied().unwrap_or(0)))
        .filter(|(_, was, now)| now < was)
        .min_by_key(|(what, _, _)| what.to_string())
        .map(|(what, was, now)| (what.to_owned(), was, now))
}
