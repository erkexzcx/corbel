use super::{Bead, Originals, samples};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug)]
struct Joint {
    before: usize,
    after: usize,
    distance: f64,
    point: (f64, f64),
    members: BTreeSet<usize>,
}

fn nearest(before: &Bead, after: &Bead) -> (f64, (f64, f64)) {
    let count = (before.span / 0.1).ceil().max(1.0) as usize;
    let distances: Vec<_> = (0..=count)
        .map(|index| after.distance(before.point(index as f64 / count as f64)))
        .collect();
    let mut best = (f64::INFINITY, before.from);
    for index in 0..=count {
        if index > 0 && distances[index - 1] < distances[index]
            || index < count && distances[index + 1] < distances[index]
        {
            continue;
        }
        let mut low = index.saturating_sub(1) as f64 / count as f64;
        let mut high = (index + 1).min(count) as f64 / count as f64;
        for _ in 0..24 {
            let left = low + (high - low) / 3.0;
            let right = high - (high - low) / 3.0;
            if after.distance(before.point(left)) < after.distance(before.point(right)) {
                high = right;
            } else {
                low = left;
            }
        }
        for share in [index as f64 / count as f64, (low + high) / 2.0] {
            let point = before.point(share);
            let distance = after.distance(point);
            if distance < best.0 {
                best = (distance, point);
            }
        }
    }
    best
}

fn find(beads: &[Bead], layer: Option<usize>, width: f64) -> BTreeMap<(usize, usize), Joint> {
    let mut cells = HashMap::<(usize, i64, i64), Vec<usize>>::new();
    let mut joints = BTreeMap::<(usize, usize), Joint>::new();
    for (index, bead) in beads.iter().enumerate().filter(|(_, bead)| {
        bead.feature.is_perimeter() && layer.is_none_or(|layer| bead.layer + 1 == layer)
    }) {
        let occupied: BTreeSet<_> = samples(bead, 0.25)
            .into_iter()
            .map(|point| (bead.layer, point.0.floor() as i64, point.1.floor() as i64))
            .collect();
        let mut candidates = BTreeSet::new();
        for &(layer, horizontal, vertical) in &occupied {
            for across in -1..=1 {
                for up in -1..=1 {
                    if let Some(found) = cells.get(&(layer, horizontal + across, vertical + up)) {
                        candidates.extend(
                            found
                                .iter()
                                .copied()
                                .filter(|&other| beads[other].run != bead.run),
                        );
                    }
                }
            }
        }
        for other in candidates {
            let previous = &beads[other];
            let (distance, point) = nearest(previous, bead);
            if distance <= width {
                let key = (previous.run, bead.run);
                let joint = joints.entry(key).or_insert_with(|| Joint {
                    before: other,
                    after: index,
                    distance,
                    point,
                    members: BTreeSet::new(),
                });
                joint.members.extend([other, index]);
                if distance < joint.distance {
                    joint.before = other;
                    joint.after = index;
                    joint.distance = distance;
                    joint.point = point;
                }
            }
        }
        for cell in occupied {
            cells.entry(cell).or_default().push(index);
        }
    }
    joints
}

fn beside(current: &[&Bead], other: &[&Bead], reach: f64) -> f64 {
    let mut near = 0.0;
    let mut total = 0.0;
    for bead in current {
        let count = (bead.span / 0.25).ceil().max(1.0) as usize;
        let weight = bead.span / count as f64;
        for index in 0..count {
            let point = bead.point((index as f64 + 0.5) / count as f64);
            total += weight;
            if other.iter().any(|other| other.distance(point) <= reach) {
                near += weight;
            }
        }
    }
    near / total.max(f64::MIN_POSITIVE)
}

pub(super) fn audit(input: &[Bead], output: &[Bead], layer: Option<usize>) {
    let originals = Originals::new(input);
    let mut pieces = HashMap::<usize, Vec<&Bead>>::new();
    let mut missing = 0;
    for bead in output {
        if let Some(original) = originals.find(bead) {
            pieces.entry(original.line).or_default().push(bead);
        } else {
            missing += 1;
        }
    }
    let mut runs = BTreeMap::<usize, Vec<&Bead>>::new();
    for bead in input.iter().filter(|bead| bead.feature.is_perimeter()) {
        runs.entry(bead.run).or_default().push(bead);
    }
    let joints = find(input, layer, 0.45);
    println!(
        "Same-layer wall run pairs within 0.45 mm: {}; unmatched output pieces: {missing}",
        joints.len()
    );
    println!("layer,run_a,run_b,line_a,line_b,x,y,distance,beside_a,beside_b,feature_a,feature_b");
    for (&(run_a, run_b), joint) in &joints {
        let before = &input[joint.before];
        let after = &input[joint.after];
        let left = &runs[&run_a];
        let right = &runs[&run_b];
        println!(
            "{},{run_a},{run_b},{},{},{:.6},{:.6},{:.6},{:.4},{:.4},{:?},{:?}",
            before.layer + 1,
            before.line,
            after.line,
            joint.point.0,
            joint.point.1,
            joint.distance,
            beside(left, right, 2.0),
            beside(right, left, 2.0),
            before.feature,
            after.feature
        );
        let mut changed_e = 0;
        let mut changed_xyz = 0;
        let mut changed_f = 0;
        let mut unmatched = 0;
        for &index in &joint.members {
            let original = &input[index];
            if let Some(found) = pieces.get(&original.line) {
                changed_e += usize::from(
                    (found.iter().map(|piece| piece.delta).sum::<f64>() - original.delta).abs()
                        > 0.000011,
                );
                changed_xyz += usize::from(found.iter().any(|piece| {
                    (piece.z - original.z).abs() > 0.001
                        || [piece.from, piece.point(0.5), piece.to]
                            .into_iter()
                            .any(|point| original.distance(point) > 0.002)
                }));
                changed_f += usize::from(
                    found
                        .iter()
                        .any(|piece| (piece.feed - original.feed).abs() > 0.001),
                );
            } else {
                unmatched += 1;
            }
        }
        let output_start = |run: &[&Bead]| {
            run.iter()
                .filter_map(|bead| pieces.get(&bead.line))
                .flatten()
                .map(|piece| piece.line)
                .min()
        };
        println!(
            " changes members={} e={changed_e} xyz={changed_xyz} f={changed_f} unmatched={unmatched} reversed={}",
            joint.members.len(),
            output_start(left)
                .zip(output_start(right))
                .is_some_and(|(left, right)| left > right)
        );
        for (run, other) in [(left, right), (right, left)] {
            let first = run[0];
            let last = run[run.len() - 1];
            let seam = other
                .iter()
                .map(|bead| bead.distance(last.to))
                .fold(f64::INFINITY, f64::min);
            let gap = (last.to.0 - first.from.0).hypot(last.to.1 - first.from.1);
            let out_first = pieces.get(&first.line).and_then(|found| found.first());
            let out_last = pieces.get(&last.line).and_then(|found| found.last());
            let out_gap = out_first
                .zip(out_last)
                .map(|(first, last)| (last.to.0 - first.from.0).hypot(last.to.1 - first.from.1));
            let out_seam = out_last.map(|last| {
                other
                    .iter()
                    .filter_map(|bead| pieces.get(&bead.line))
                    .flatten()
                    .map(|bead| bead.distance(last.to))
                    .fold(f64::INFINITY, f64::min)
            });
            let curve: f64 = run
                .iter()
                .filter(|bead| bead.arc.is_some())
                .map(|bead| bead.span)
                .sum();
            let length: f64 = run.iter().map(|bead| bead.span).sum();
            println!(
                " seam run={} first_line={} first_from={:?} first_to={:?} end_line={} gap={gap:.6} neighbour={seam:.6} output_gap={out_gap:?} output_neighbour={out_seam:?} length={length:.3} arc_length={curve:.3}",
                first.run, first.line, first.from, first.to, last.line
            );
        }
        for bead in [before, after] {
            if let Some(found) = pieces.get(&bead.line) {
                let delta: f64 = found.iter().map(|piece| piece.delta).sum();
                let span: f64 = found.iter().map(|piece| piece.span).sum();
                let low_z = found
                    .iter()
                    .map(|piece| piece.z)
                    .fold(f64::INFINITY, f64::min);
                let high_z = found
                    .iter()
                    .map(|piece| piece.z)
                    .fold(f64::NEG_INFINITY, f64::max);
                let low_f = found
                    .iter()
                    .map(|piece| piece.feed)
                    .fold(f64::INFINITY, f64::min);
                let high_f = found
                    .iter()
                    .map(|piece| piece.feed)
                    .fold(f64::NEG_INFINITY, f64::max);
                println!(
                    " bead line={} out_first={} out_last={} pieces={} input_e={:.5} output_e={delta:.5} density={:.6} input_z={:.6} output_z={low_z:.6}..{high_z:.6} input_f={:.3} output_f={low_f:.3}..{high_f:.3} from={:?}->{:?} to={:?}->{:?}",
                    bead.line,
                    found[0].line,
                    found[found.len() - 1].line,
                    found.len(),
                    bead.delta,
                    delta / span / (bead.delta / bead.span),
                    bead.z,
                    bead.feed,
                    bead.from,
                    found[0].from,
                    bead.to,
                    found[found.len() - 1].to
                );
            } else {
                println!(" missing input line={}", bead.line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::beads_in;
    use super::*;

    #[test]
    fn junction_screen_finds_a_circle_beside_a_square_without_grouping_them() {
        let source = "M83\n;LAYER_CHANGE\n;TYPE:Internal perimeter\nG1 X0 Y0 Z0.2\nG1 X20 Y0 E1\nG1 X20 Y20 E1\nG1 X0 Y20 E1\nG1 X0 Y0 E1\nG1 X30.4 Y10\nG3 X20.4 Y10 I-5 J0 E1\nG3 X30.4 Y10 I5 J0 E1\n";
        let beads = beads_in(source);
        let joints = find(&beads, Some(1), 0.45);
        assert_eq!(joints.len(), 1);
        let joint = joints.values().next().unwrap();
        assert!((joint.distance - 0.4).abs() < 1e-6);
        assert_eq!(joint.members.len(), 3);
        let square: Vec<_> = beads[..4].iter().collect();
        let circle: Vec<_> = beads[4..].iter().collect();
        assert!(beside(&square, &circle, 2.0) < 0.5);
        assert!(beside(&circle, &square, 2.0) < 0.5);
        assert!(find(&beads, Some(2), 0.45).is_empty());
        assert!(
            find(
                &beads_in(&source.replace("20.4", "20.6").replace("30.4", "30.6")),
                None,
                0.45
            )
            .is_empty()
        );
        let matched = Originals::new(&beads);
        assert!(
            beads
                .iter()
                .all(|bead| matched.find(bead).unwrap().line == bead.line)
        );
    }
}
