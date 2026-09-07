use clap::{Parser, ValueEnum};
use corbel::gcode::feature::{Feature, is_layer_marker};
use corbel::gcode::{Code, Extruder, Line, Modal};
use corbel::geometry::{Arc, footprint, turn};
use std::collections::{BTreeMap, HashMap};

#[path = "../../../../tests/nozzle/mod.rs"]
mod nozzle;

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Mode {
    Gap,
    Paths,
    Settings,
    Travels,
    Geometry,
    Nozzle,
}

#[derive(Parser)]
#[command(about = "Compare input and processed G-code without modifying either file")]
struct Arguments {
    #[arg(value_enum)]
    mode: Mode,
    input: String,
    output: String,
}

#[derive(Clone, Debug)]
struct Bead {
    layer: usize,
    from: (f64, f64),
    to: (f64, f64),
    z: f64,
    delta: f64,
    span: f64,
    arc: Option<Arc>,
    feature: Feature,
    line: usize,
    settings: BTreeMap<String, String>,
}

impl Bead {
    fn point(&self, share: f64) -> (f64, f64) {
        match self.arc.and_then(|arc| turn(self.from, self.to, arc)) {
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
        let distance = |other: (f64, f64)| (point.0 - other.0).hypot(point.1 - other.1);
        if let Some((centre, radius, start, sweep)) =
            self.arc.and_then(|arc| turn(self.from, self.to, arc))
        {
            let angle = (point.1 - centre.1).atan2(point.0 - centre.0);
            let angle = if sweep > 0.0 {
                angle - start
            } else {
                start - angle
            }
            .rem_euclid(std::f64::consts::TAU);
            if angle <= sweep.abs() {
                return (distance(centre) - radius).abs();
            }
            return distance(self.from).min(distance(self.to));
        }
        if self.span == 0.0 {
            return distance(self.from);
        }
        let direction = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let share = ((point.0 - self.from.0) * direction.0 + (point.1 - self.from.1) * direction.1)
            / self.span.powi(2);
        distance(self.point(share.clamp(0.0, 1.0)))
    }

    fn contains(&self, piece: &Bead) -> Option<f64> {
        if piece.span > self.span + 0.03 {
            return None;
        }
        match (self.arc, piece.arc) {
            (None, None) => {
                let dot = (self.to.0 - self.from.0) * (piece.to.0 - piece.from.0)
                    + (self.to.1 - self.from.1) * (piece.to.1 - piece.from.1);
                if dot < 0.0 {
                    return None;
                }
            }
            (Some(before), Some(after)) if before.clockwise == after.clockwise => {}
            _ => return None,
        }
        let error = [piece.from, piece.point(0.5), piece.to]
            .into_iter()
            .map(|point| self.distance(point))
            .fold(0.0_f64, f64::max);
        (error <= 0.03).then_some(error)
    }
}

struct Originals<'a> {
    cells: HashMap<(usize, i64, i64), Vec<&'a Bead>>,
}

impl<'a> Originals<'a> {
    fn new(beads: &'a [Bead]) -> Self {
        let mut cells = HashMap::<_, Vec<&Bead>>::new();
        for bead in beads {
            for point in samples(bead, 1.0) {
                let entries = cells
                    .entry((
                        bead.layer,
                        (point.0 / 2.0).floor() as i64,
                        (point.1 / 2.0).floor() as i64,
                    ))
                    .or_default();
                if entries.last().is_none_or(|last| last.line != bead.line) {
                    entries.push(bead);
                }
            }
        }
        Self { cells }
    }

    fn find(&self, bead: &Bead) -> Option<&'a Bead> {
        let point = bead.point(0.5);
        let cell = (
            (point.0 / 2.0).floor() as i64,
            (point.1 / 2.0).floor() as i64,
        );
        let mut best = None;
        let mut error = f64::INFINITY;
        for horizontal in -1..=1 {
            for vertical in -1..=1 {
                if let Some(candidates) =
                    self.cells
                        .get(&(bead.layer, cell.0 + horizontal, cell.1 + vertical))
                {
                    for original in candidates {
                        if let Some(distance) = original.contains(bead) {
                            if distance < error {
                                best = Some(*original);
                                error = distance;
                            }
                            if distance < 1e-9 {
                                return best;
                            }
                        }
                    }
                }
            }
        }
        best
    }
}

fn beads(path: &str) -> std::io::Result<Vec<Bead>> {
    let bytes = std::fs::read(path)?;
    Ok(beads_in(&String::from_utf8_lossy(&bytes)))
}

fn beads_in(text: &str) -> Vec<Bead> {
    let mut modal = Modal::new();
    let mut extruder = Extruder::new();
    let mut feature = Feature::Other;
    let mut layer = 0;
    let mut started = false;
    let mut beads = Vec::new();
    let mut settings = BTreeMap::new();
    for (index, raw) in text.lines().enumerate() {
        let line = Line::parse(raw);
        let command = raw.split(';').next().unwrap_or_default();
        let words = command.split_ascii_whitespace().collect::<Vec<_>>();
        if let Some(&code) = words.first() {
            if code == "M204" {
                settings.insert(code.to_owned(), words.join(" "));
            } else if code == "M106" || code == "M107" {
                let channel = words
                    .iter()
                    .find_map(|word| word.strip_prefix('P'))
                    .unwrap_or("0");
                let speed = if code == "M107" {
                    "0"
                } else {
                    words
                        .iter()
                        .find_map(|word| word.strip_prefix('S'))
                        .unwrap_or("255")
                };
                settings.insert(format!("fan{channel}"), speed.to_owned());
            }
        }
        if let Some(marker) = line.marker() {
            if is_layer_marker(marker) {
                layer += usize::from(std::mem::replace(&mut started, true));
            } else if let Some(found) = Feature::from_marker(marker) {
                feature = found;
            }
        }
        let origin = modal.position();
        modal.apply(&line);
        match line.code {
            Code::AbsoluteE | Code::RelativeE => extruder.set_mode(line.code),
            Code::SetPosition => {
                if let Some(value) = line.e {
                    extruder.observe_origin(value);
                }
            }
            _ => {}
        }
        let Some(value) = line.e.filter(|_| line.draws()) else {
            continue;
        };
        let delta = extruder.observe(value);
        if !started || delta <= 0.0 || !line.draws_in_plane() {
            continue;
        }
        let destination = modal.position();
        let from = (origin.0, origin.1);
        let to = (destination.0, destination.1);
        let arc = line.arc_between(from, to);
        let span = arc.and_then(|arc| turn(from, to, arc)).map_or_else(
            || (to.0 - from.0).hypot(to.1 - from.1),
            |(_, radius, _, sweep)| radius * sweep.abs(),
        );
        beads.push(Bead {
            layer,
            from,
            to,
            z: destination.2,
            delta,
            span,
            arc,
            feature,
            line: index + 1,
            settings: settings.clone(),
        });
    }
    beads
}

fn samples(bead: &Bead, step: f64) -> Vec<(f64, f64)> {
    let curve = bead.arc.and_then(|arc| turn(bead.from, bead.to, arc));
    let count = (bead.span / step).ceil().max(1.0) as usize;
    (0..=count)
        .map(|index| {
            let share = index as f64 / count as f64;
            match curve {
                Some((centre, radius, start, sweep)) => {
                    let angle = start + sweep * share;
                    (
                        centre.0 + radius * angle.cos(),
                        centre.1 + radius * angle.sin(),
                    )
                }
                None => (
                    bead.from.0 + (bead.to.0 - bead.from.0) * share,
                    bead.from.1 + (bead.to.1 - bead.from.1) * share,
                ),
            }
        })
        .collect()
}

fn gap_fractions(area_ratio: f64, gaps: &[f64], flow: f64) -> (f64, f64) {
    let over = gaps
        .iter()
        .filter(|gap| area_ratio > **gap * flow * 1.2 + 0.05)
        .count();
    let under = gaps
        .iter()
        .filter(|gap| area_ratio + 0.05 < **gap * flow * 0.8)
        .count();
    let count = gaps.len().max(1) as f64;
    (over as f64 / count, under as f64 / count)
}

fn audit_gap(input: &[Bead], output: &[Bead], width: f64) {
    let mut planes = BTreeMap::<usize, f64>::new();
    let originals = Originals::new(input);
    for bead in input {
        planes
            .entry(bead.layer)
            .and_modify(|plane| *plane = plane.min(bead.z))
            .or_insert(bead.z);
    }
    let mut ground = HashMap::<(usize, i64, i64), Vec<&Bead>>::new();
    for bead in output {
        for (x, y) in samples(bead, 0.10) {
            let entries = ground
                .entry((
                    bead.layer,
                    (x / 0.4).floor() as i64,
                    (y / 0.4).floor() as i64,
                ))
                .or_default();
            if entries.last().is_none_or(|last| last.line != bead.line) {
                entries.push(bead);
            }
        }
    }
    let mut suspect = Vec::new();
    let mut counts = BTreeMap::<String, (usize, f64, f64, f64, f64)>::new();
    for bead in output {
        let Some(original) = originals.find(bead) else {
            continue;
        };
        if bead.layer == 0 || bead.span < 0.5 {
            continue;
        }
        let height = planes[&bead.layer] - planes[&(bead.layer - 1)];
        if height <= 0.0 {
            continue;
        }
        let flow = if bead.feature.is_perimeter() {
            corbel::brick::automatic_flow(height, Some(width), 0.05)
        } else {
            1.0
        };
        let area_ratio = (bead.delta / bead.span) / (original.delta / original.span);
        let kind = if bead.arc.is_some() { "arc" } else { "line" };
        let totals = counts
            .entry(format!("{:?}/{kind}", bead.feature))
            .or_default();
        totals.0 += 1;
        totals.1 = totals.1.max(area_ratio);
        let count = (bead.span / 0.1).ceil().max(1.0) as usize;
        let points: Vec<_> = (0..count)
            .map(|index| bead.point((index as f64 + 0.5) / count as f64))
            .collect();
        let mut gaps = Vec::new();
        let mut unsupported = 0;
        for (x, y) in &points {
            let spacing = width - height * (1.0 - std::f64::consts::FRAC_PI_4);
            let reach = spacing / 2.0;
            let normal = match bead.arc.and_then(|arc| turn(bead.from, bead.to, arc)) {
                Some((centre, radius, _, _)) => ((x - centre.0) / radius, (y - centre.1) / radius),
                None => (
                    (bead.from.1 - bead.to.1) / bead.span,
                    (bead.to.0 - bead.from.0) / bead.span,
                ),
            };
            let mut rise = 0.0;
            for across in 0..11 {
                let offset = reach * (2.0 * (across as f64 + 0.5) / 11.0 - 1.0);
                let sample = (x + normal.0 * offset, y + normal.1 * offset);
                let cell = (
                    (sample.0 / 0.4).floor() as i64,
                    (sample.1 / 0.4).floor() as i64,
                );
                let mut nearest = (f64::INFINITY, planes[&(bead.layer - 1)]);
                for horizontal in -1..=1 {
                    for vertical in -1..=1 {
                        if let Some(below) =
                            ground.get(&(bead.layer - 1, cell.0 + horizontal, cell.1 + vertical))
                        {
                            for path in below {
                                let distance = path.distance(sample);
                                if distance < nearest.0 {
                                    nearest = (distance, path.z);
                                }
                            }
                        }
                    }
                }
                if across == 5 && nearest.0 > width {
                    unsupported += 1;
                }
                if nearest.0 < reach {
                    rise += (nearest.1 - planes[&(bead.layer - 1)]) / 11.0;
                }
            }
            gaps.push((height + bead.z - planes[&bead.layer] - rise).max(0.0) / height);
        }
        let average = gaps.iter().sum::<f64>() / gaps.len() as f64;
        let (over, under) = gap_fractions(area_ratio, &gaps, flow);
        totals.2 += over * bead.span;
        totals.3 += under * bead.span;
        totals.4 += unsupported as f64 / points.len() as f64 * bead.span;
        if over > 0.1 || under > 0.1 {
            suspect.push((
                (over + under) * bead.span,
                bead,
                area_ratio,
                average * flow,
                over,
                under,
                unsupported as f64 / points.len() as f64,
                gaps.iter().copied().fold(f64::INFINITY, f64::min),
                gaps.iter().copied().fold(0.0, f64::max),
            ));
        }
    }
    println!(
        "Geometric screening (count, max area ratio, path above estimate, path below estimate, path without nearby ground): {counts:?}"
    );
    suspect.sort_by(|left, right| right.0.total_cmp(&left.0));
    for (_, bead, ratio, expected, over, under, unsupported, minimum, maximum) in
        suspect.iter().take(30)
    {
        println!(
            "line={} layer={} arc={} span={:.3} written_factor={ratio:.4} geometric_mean={expected:.4} local_gap={minimum:.4}..{maximum:.4} over={over:.3} under={under:.3} no_near_ground={unsupported:.3} feature={:?} from={:?} to={:?}",
            bead.line,
            bead.layer,
            bead.arc.is_some(),
            bead.span,
            bead.feature,
            bead.from,
            bead.to
        );
    }
}

#[derive(Debug)]
struct Travel {
    layer: usize,
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    before: f64,
    after: f64,
    delta: Option<f64>,
    span: f64,
    arc: Option<Arc>,
    line: usize,
}

fn travels(path: &str) -> std::io::Result<Vec<Travel>> {
    let bytes = std::fs::read(path)?;
    Ok(travels_in(&String::from_utf8_lossy(&bytes)))
}

fn travels_in(text: &str) -> Vec<Travel> {
    let mut modal = Modal::new();
    let mut extruder = Extruder::new();
    let mut withdrawn = 0.0_f64;
    let mut layer = 0;
    let mut bead_layer = 0;
    let mut started = false;
    let mut moves = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = Line::parse(raw);
        if line.marker().is_some_and(is_layer_marker) {
            layer += usize::from(std::mem::replace(&mut started, true));
        }
        match line.code {
            Code::AbsoluteE | Code::RelativeE => extruder.set_mode(line.code),
            Code::SetPosition => {
                if let Some(value) = line.e {
                    extruder.observe_origin(value);
                }
            }
            _ => {}
        }
        let from = modal.position();
        modal.apply(&line);
        let to = modal.position();
        let before = withdrawn;
        let delta = line
            .e
            .filter(|_| line.draws())
            .map(|value| extruder.observe(value));
        if let Some(delta) = delta {
            withdrawn = (withdrawn - delta).max(0.0);
        }
        if line.draws_in_plane() && delta.is_some_and(|value| value > 0.0) {
            bead_layer = layer;
        }
        if !started || !line.draws_in_plane() || delta.is_some_and(|value| value > 0.0) {
            continue;
        }
        let arc = line.arc_between((from.0, from.1), (to.0, to.1));
        let span = footprint::along((from.0, from.1), (to.0, to.1), arc);
        let belongs = if delta.is_some() { bead_layer } else { layer };
        moves.push(Travel {
            layer: belongs,
            from,
            to,
            before,
            after: withdrawn,
            delta,
            span,
            arc,
            line: index + 1,
        });
    }
    moves
}

fn main() -> std::io::Result<()> {
    let args = Arguments::parse();
    if args.mode == Mode::Nozzle {
        let original = std::fs::read(&args.input)?;
        let processed = std::fs::read(&args.output)?;
        let original = String::from_utf8_lossy(&original);
        let processed = String::from_utf8_lossy(&processed);
        let before = nozzle::ledger(&original);
        let after = nozzle::ledger(&processed);
        let faults = nozzle::faults(&before, &after, None);
        println!("Nozzle ledger faults: {}", faults.len());
        println!(
            "Excess prime: {:.5} mm; dry bead: {:.5} mm; primed travel: {:.3} mm",
            after.excess_prime, after.dry_bead, after.primed_travel
        );
        for fault in &faults {
            println!("{fault}");
        }
        return if faults.is_empty() {
            Ok(())
        } else {
            Err(std::io::Error::other("nozzle audit failed"))
        };
    }
    if args.mode == Mode::Travels {
        let original = travels(&args.input)?;
        for path in [&args.input, &args.output] {
            let travel = travels(path)?;
            for threshold in [0.000001, 0.0001, 0.1, 0.4, 0.7999] {
                let long = travel
                    .iter()
                    .filter(|motion| motion.span > 1.0 && motion.before < threshold)
                    .collect::<Vec<_>>();
                println!(
                    "{path}: withdrawal<{threshold} long_moves={} length={:.3} maximum={:.3}",
                    long.len(),
                    long.iter().map(|motion| motion.span).sum::<f64>(),
                    long.iter()
                        .map(|motion| motion.span)
                        .fold(0.0_f64, f64::max)
                );
            }
        }
        let output = travels(&args.output)?;
        let near = |before: &Travel, motion: &Travel| {
            before.layer == motion.layer
                && (before.from.0 - motion.from.0).hypot(before.from.1 - motion.from.1) < 0.03
                && (before.to.0 - motion.to.0).hypot(before.to.1 - motion.to.1) < 0.03
                && match (before.arc, motion.arc) {
                    (Some(left), Some(right)) => {
                        left.clockwise == right.clockwise
                            && (left.i - right.i).hypot(left.j - right.j) < 0.03
                    }
                    (None, None) => true,
                    _ => false,
                }
        };
        let mut changed = output
            .iter()
            .filter(|motion| motion.span > 1.0 && motion.before < 0.7999)
            .filter(|motion| !original.iter().any(|before| near(before, motion)))
            .collect::<Vec<_>>();
        changed.sort_by(|left, right| right.span.total_cmp(&left.span));
        println!("New incompletely retracted travels: {}", changed.len());
        for motion in changed.iter().take(20) {
            println!(
                "line={} delta={:?} withdrawal_after={} {motion:?}",
                motion.line, motion.delta, motion.after
            );
        }
        let mut lowered = Vec::new();
        for motion in &output {
            if let Some(before) = original
                .iter()
                .filter(|before| near(before, motion))
                .min_by(|left, right| {
                    let diff = |before: &Travel| {
                        (before.to.2 - motion.to.2).abs() + (before.from.2 - motion.from.2).abs()
                    };
                    diff(left).total_cmp(&diff(right))
                })
            {
                let drop = (before.from.2 - motion.from.2).max(before.to.2 - motion.to.2);
                if drop > 0.001 {
                    lowered.push((drop, motion, before));
                }
            }
        }
        lowered.sort_by(|left, right| right.0.total_cmp(&left.0));
        println!("Same-path travels lowered: {}", lowered.len());
        for (drop, after, before) in lowered.iter().take(15) {
            println!("drop={drop}\nbefore={before:?}\nafter={after:?}");
        }
        return Ok(());
    }
    let input = beads(&args.input)?;
    let output = beads(&args.output)?;
    if args.mode == Mode::Gap {
        let source = std::fs::read(&args.input)?;
        let width = nozzle::Nozzle::read(&String::from_utf8_lossy(&source)).bead;
        audit_gap(&input, &output, width);
        return Ok(());
    }
    if matches!(args.mode, Mode::Paths | Mode::Settings) {
        let check_settings = args.mode == Mode::Settings;
        let originals = Originals::new(&input);
        let mut changed = Vec::new();
        for bead in &output {
            let best = originals.find(bead);
            if best.is_none()
                || (check_settings && best.is_some_and(|before| before.settings != bead.settings))
            {
                changed.push((bead, best));
            }
        }
        println!(
            "{} mismatches: {} of {}",
            if check_settings {
                "Settings"
            } else {
                "Printing path"
            },
            changed.len(),
            output.len()
        );
        for (bead, original) in changed.iter().take(25) {
            println!("output={bead:?}\ninput={original:?}");
        }
        return if changed.is_empty() {
            Ok(())
        } else {
            Err(std::io::Error::other("path/settings audit failed"))
        };
    }
    if args.mode == Mode::Geometry {
        let geometry = |beads: &[Bead]| {
            beads
                .iter()
                .map(|bead| {
                    (
                        bead.layer,
                        bead.from,
                        bead.to,
                        bead.z,
                        format!("{:?}", bead.arc),
                        bead.feature,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(geometry(&input), geometry(&output));
        println!(
            "Identical bead geometry and ordering: {} beads",
            input.len()
        );
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gap_screen_detects_excess_and_missing_filament() {
        assert_eq!(gap_fractions(1.0, &[0.5, 1.0], 1.0), (0.5, 0.0));
        assert_eq!(gap_fractions(0.5, &[0.5, 1.0], 1.0), (0.0, 0.5));
        assert_eq!(gap_fractions(1.025, &[1.0, 1.0], 1.025), (0.0, 0.0));
        assert_eq!(gap_fractions(1.0, &[], 1.0), (0.0, 0.0));
    }

    #[test]
    fn ground_distance_reaches_between_samples_and_around_arcs() {
        let straight = beads_in("M83\n;LAYER_CHANGE\nG1 X0 Y0\nG1 X10 Y0 E1\n");
        assert!((straight[0].distance((0.05, 0.2)) - 0.2).abs() < 1e-12);
        let curved = beads_in("M83\n;LAYER_CHANGE\nG1 X10 Y0\nG3 X-10 Y0 I-10 J0 E1\n");
        let angle = 0.005_f64;
        assert!((curved[0].distance((10.2 * angle.cos(), 10.2 * angle.sin())) - 0.2).abs() < 1e-12);
    }

    #[test]
    fn circles_are_measured_round_the_curve_not_across_the_chord() {
        let input = beads_in("M83\n;LAYER_CHANGE\nG1 X10 Y0 Z0.2\nG3 X-10 Y0 I-10 J0 E1\n");
        assert_eq!(input.len(), 1);
        assert!((input[0].span - std::f64::consts::PI * 10.0).abs() < 1e-9);
        assert_eq!(input[0].delta, 1.0);
    }

    #[test]
    fn a_wipe_keeps_the_layer_of_its_preceding_bead() {
        let input = travels_in(
            "M83\n;LAYER_CHANGE\nG1 X0 Y0 Z0.2\nG1 X10 Y0 E1\n;LAYER_CHANGE\nG1 X9 Y0 E-.8\n",
        );
        let wipe = input
            .iter()
            .find(|motion| motion.delta == Some(-0.8))
            .unwrap();
        assert_eq!(wipe.layer, 0);
        assert_eq!(wipe.from, (10.0, 0.0, 0.2));
        assert_eq!(wipe.after, 0.8);
    }

    #[test]
    fn a_spiral_hop_is_still_a_travel_without_endpoint_words() {
        let input = travels_in("M83\n;LAYER_CHANGE\nG1 X0 Y0 Z0.2\nG3 I1 J0 Z0.6\n");
        let hop = input.last().unwrap();
        assert!((hop.span - std::f64::consts::TAU).abs() < 1e-9);
        assert_eq!(hop.from.2, 0.2);
        assert_eq!(hop.to.2, 0.6);
    }

    #[test]
    fn split_moves_match_their_original_path_but_shortcuts_do_not() {
        let source = beads_in("M83\n;LAYER_CHANGE\nG1 X0 Y0\nG1 X10 Y0 E1\n");
        let pieces = beads_in("M83\n;LAYER_CHANGE\nG1 X0 Y0\nG1 X4 Y0 E.2\nG1 X10 Y0 E.6\n");
        let bad = beads_in("M83\n;LAYER_CHANGE\nG1 X0 Y1\nG1 X4 Y0 E.2\n");
        let originals = Originals::new(&source);
        assert!(pieces.iter().all(|piece| originals.find(piece).is_some()));
        assert!(originals.find(&bad[0]).is_none());
    }
}
