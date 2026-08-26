//! What the binary writes, replayed through a nozzle.
//!
//! Everything else in the suite reads the output: it counts stamps, compares
//! `E` words, checks a height against a plane. None of that can see the defect
//! these tests exist for, because the defect is not in any one line — it is in
//! where a line puts the toolhead relative to material some *other* line
//! already laid. Only replaying the file can find that, so these tests state
//! the two physical rules once and let [`nozzle`] decide whether the file
//! keeps them.
//!
//! The fixtures are real slices, not hand-written ones. A hand-written file
//! has no wipes, no seam gaps, no arcs and no gap fill, and the wall order it
//! is written in is whatever the author assumed — which is how a suite ends up
//! only ever exercising the order no mainstream slicer ships.

mod nozzle;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};
use std::{env, fs};

const BIN: &str = env!("CARGO_BIN_EXE_corbel");

/// A real slice, one hidden wall behind the visible one.
const TWO_WALLS: &str = "objects_2walls_bambu.gcode.zst";

/// The same plate carrying as many walls as its geometry allows, so the hidden
/// walls are laid against each other and not only against the visible one.
const MANY_WALLS: &str = "objects_manywalls_bambu.gcode.zst";

/// Every input and switch the two rules are stated over.
///
/// Both of those tests walk this same list, so whichever runs second pays
/// nothing. The surface transform meets a real slice through `--bricks --zaa`
/// rather than twice over: unoptimised it is by far the slowest thing the
/// binary does, and a run naming it alone is covered on the stack, which is
/// small enough to be free.
const EVERY_RUN: &[(&str, &[&str])] = &[
    (TWO_WALLS, &["--bricks"]),
    (TWO_WALLS, &["--bricks", "--zaa"]),
    (MANY_WALLS, &["--bricks"]),
    (MANY_WALLS, &["--bricks", "--zaa"]),
    ("stack/3/last", &["--bricks"]),
    ("stack/3/last", &["--zaa"]),
    ("stack/3/last", &["--bricks", "--zaa"]),
];

/// The runs the bricking transform answers for on its own.
const BRICKED_RUNS: &[(&str, &[&str])] = &[
    (TWO_WALLS, &["--bricks"]),
    (MANY_WALLS, &["--bricks"]),
    ("stack/3/last", &["--bricks"]),
];

/// The runs the surface transform answers for. `--zaa` on its own follows the
/// hidden walls too, and where one of a pair is covered by the layer above and
/// the other is not, the two are laid half a layer apart. The pairing this
/// measures is the one the tool recommends.
const SURFACE_RUNS: &[(&str, &[&str])] = &[
    (TWO_WALLS, &["--bricks", "--zaa"]),
    (MANY_WALLS, &["--bricks", "--zaa"]),
    ("stack/3/last", &["--zaa"]),
];

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("corbel-{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create sandbox");
        Self(path)
    }

    fn holding(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).expect("write input");
        path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let packed = fs::read(&path).unwrap_or_else(|why| panic!("read {}: {why}", path.display()));
    let mut text = String::new();
    ruzstd::decoding::StreamingDecoder::new(packed.as_slice())
        .unwrap_or_else(|why| panic!("open {}: {why}", path.display()))
        .read_to_string(&mut text)
        .unwrap_or_else(|why| panic!("unpack {}: {why}", path.display()));
    text
}

/// The input one of these names, which is either a stored slice or a stack
/// built to order.
fn source(name: &str) -> String {
    match name.strip_prefix("stack/") {
        Some(rest) => {
            let (walls, order) = rest.split_once('/').expect("stack/<walls>/<order>");
            stack(walls.parse().expect("wall count"), order == "first")
        }
        None => fixture(name),
    }
}

/// What the binary wrote, and what a nozzle makes of it.
struct Run {
    gcode: String,
    report: nozzle::Report,
}

/// One input through one set of switches, worked out once however many tests
/// ask for it. Several of them want the same file and the binary is being run
/// unoptimised over a real slice, so the saving is most of the suite.
fn run_of(name: &str, args: &[&str]) -> &'static Run {
    static DONE: LazyLock<Mutex<HashMap<String, &'static Run>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let key = format!("{name} {args:?}");
    // Held across the run so that two tests asking at once wait rather than
    // both paying for it.
    let mut done = DONE.lock().expect("cache");
    if let Some(had) = done.get(&key) {
        return had;
    }
    let gcode = processed(name, &source(name), args);
    let report = nozzle::inspect(&gcode);
    let run: &'static Run = Box::leak(Box::new(Run { gcode, report }));
    done.insert(key, run);
    run
}

/// Runs the binary over `source` and hands back what it wrote.
fn processed(label: &str, source: &str, args: &[&str]) -> String {
    let sandbox = Sandbox::new(&label.replace(['/', '.'], "-"));
    let input = sandbox.holding("part.gcode", source);
    let output = sandbox.0.join("out.gcode");
    let run = Command::new(BIN)
        .args(args)
        .arg("--output")
        .arg(&output)
        .arg(&input)
        .output()
        .expect("run binary");
    assert!(
        run.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::read_to_string(&output).expect("read output")
}

/// The region in force at each line, so a plunge can be blamed on the wall it
/// happened in. Only a bare comment is a marker: the stamps this tool leaves
/// ride the moves it rewrites.
fn regions(gcode: &str) -> Vec<String> {
    let mut current = String::new();
    gcode
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed
                .strip_prefix(";TYPE:")
                .or_else(|| trimmed.strip_prefix("; FEATURE:"))
            {
                current = rest.trim().to_ascii_lowercase();
            }
            current.clone()
        })
        .collect()
}

/// Eight layers of PrusaSlicer-flavoured output carrying `walls` concentric
/// loops, printed in whichever order is asked for.
///
/// Both orders exist because the order decides the answer and neither can be
/// assumed: every mainstream slicer ships the visible wall last, and the
/// suite's other fixtures are all written the other way round.
fn stack(walls: usize, external_first: bool) -> String {
    const SIZE: f64 = 20.0;
    /// mm of filament a mm of bead asks for, at 0.45 x 0.2 through 1.75 stock.
    const FLOW: f64 = 0.0322;
    /// Where a slicer really puts the loop behind the visible one, and where
    /// it puts each hidden loop after that: `w - h(1 - PI/4)`, not the nominal
    /// width. Measured on the stored slices at 0.391 mm and 0.407 mm. Spacing
    /// at the nominal width instead puts the neighbour 0.45 mm away, which is
    /// just outside the reach of the nozzle laying the bead — so a fixture
    /// written that way reports no collision where a real file has one on
    /// every layer.
    const SKIN_GAP: f64 = 0.392;
    const WALL_GAP: f64 = 0.407;

    let inset = |loop_: usize| match loop_ {
        0 => 0.0,
        _ => SKIN_GAP + (loop_ - 1) as f64 * WALL_GAP,
    };

    let mut text = format!(
        "; generated by PrusaSlicer\n\
         M83 ; extruder relative mode\n\
         ; layer_height = 0.2\n\
         ; nozzle_diameter = 0.4\n\
         ; perimeter_extrusion_width = 0.45\n\
         ; external_perimeter_extrusion_width = 0.42\n\
         ; external_perimeters_first = {}\n",
        u8::from(external_first)
    );
    for layer in 0..8 {
        let z = 0.2 * (layer + 1) as f64;
        text.push_str(";LAYER_CHANGE\n");
        text.push_str(&format!("G1 Z{z:.3} F9000\n"));
        let order: Vec<usize> = match external_first {
            true => (0..walls).collect(),
            false => (0..walls).rev().collect(),
        };
        for loop_ in order {
            text.push_str(match loop_ {
                0 => ";TYPE:External perimeter\n",
                _ => ";TYPE:Perimeter\n",
            });
            let ring = ring(inset(loop_), SIZE);
            let (sx, sy) = ring[0];
            text.push_str(&format!("G1 X{sx:.3} Y{sy:.3} F9000\n"));
            for pair in ring.windows(2) {
                let span = (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1);
                text.push_str(&format!(
                    "G1 X{:.3} Y{:.3} E{:.5}\n",
                    pair[1].0,
                    pair[1].1,
                    span * FLOW
                ));
            }
        }
        text.push_str(match layer == 0 || layer == 7 {
            true => ";TYPE:Solid infill\n",
            false => ";TYPE:Internal infill\n",
        });
        text.push_str("G1 X3.000 Y3.000 F9000\n");
        for line in 0..8 {
            let y = 3.0 + line as f64 * 2.0;
            let (from, to) = match line % 2 {
                0 => (3.0, 17.0),
                _ => (17.0, 3.0),
            };
            text.push_str(&format!("G1 X{from:.3} Y{y:.3} F9000\n"));
            text.push_str(&format!("G1 X{to:.3} Y{y:.3} E{:.5}\n", 14.0 * FLOW));
        }
    }
    text.push_str("M104 S0\n");
    text
}

/// One closed loop, walked at about a millimetre a step and stopped short of
/// its own seam the way a slicer stops one.
fn ring(inset: f64, size: f64) -> Vec<(f64, f64)> {
    let near = inset;
    let far = size - inset;
    let corners = [
        (near, near),
        (far, near),
        (far, far),
        (near, far),
        (near, near + 0.04),
    ];
    let mut points = vec![corners[0]];
    for pair in corners.windows(2) {
        let span = (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1);
        let steps = (span.ceil() as usize).max(1);
        for step in 1..=steps {
            let share = step as f64 / steps as f64;
            points.push((
                pair[0].0 + (pair[1].0 - pair[0].0) * share,
                pair[0].1 + (pair[1].1 - pair[0].1) * share,
            ));
        }
    }
    points
}

/// The model has to be silent on the files this all started from, or nothing
/// it says about a processed one means anything. A slicer's own output puts
/// the nozzle at the plane its layer sits on and every bead it has already
/// laid tops out at exactly that plane, so there is nothing to plow.
#[test]
fn the_slices_the_fixtures_came_from_are_already_clear() {
    for name in [TWO_WALLS, MANY_WALLS] {
        let source = fixture(name);
        // The reason for storing a slice rather than writing one: a file this
        // was replaced by would have none of these, and the defects the rest
        // of this file is about live in exactly this machinery.
        for wanted in [
            "\nG2 ",
            "\nG3 ",
            "WIPE_START",
            "; FEATURE:",
            "; CHANGE_LAYER",
        ] {
            assert!(
                source.contains(wanted),
                "{name} no longer looks like slicer output: no {wanted:?}"
            );
        }
        let report = nozzle::inspect(&source);
        assert!(
            report.beads > 5_000,
            "{name} is too small to prove anything: {} beads",
            report.beads
        );
        report.assert_clear(name);
    }
}

/// The rule, stated once, over every combination the binary offers.
///
/// This is the net the rest of the file hangs off: a transform can change how
/// it groups loops, numbers them, meters them or writes them, and it still has
/// to hand back a file a nozzle can execute.
#[test]
fn no_run_ever_puts_the_nozzle_through_what_it_has_already_printed() {
    for (name, args) in BRICKED_RUNS {
        run_of(name, args)
            .report
            .assert_clear(&format!("{name} {args:?}"));
    }
}

/// How deep a crest the surface transform may leave, as a share of the layer.
///
/// Half a layer is its own amplitude: it moves a bead by up to that either way
/// from the plane, so a bead at one extreme beside one left on the plane is
/// the worst a single run can build.
const SURFACE_CREST: f64 = 0.5;

/// The same rule for the surface transform, held to its amplitude rather than
/// to zero.
///
/// Zero is not reachable, and that is measured rather than assumed. Following
/// a surface *inside* a layer means the beads of that layer are no longer all
/// at one height; two passes of a top surface run about 0.4 mm apart, which is
/// inside the nozzle's own underside; and the two cross in height along their
/// own length, so there is no order of whole passes in which every point
/// ascends. Six ways of removing it were tried on the stored plate and every
/// one is recorded in README.md with what it did — the best of them took 540
/// crests to 481 and made the median deeper.
///
/// What this holds is the discipline that got the worst case inside the
/// amplitude at all: the climb bounded to one layer height per bead width
/// where it had been one per grid cell, a stretch of exposure too narrow to
/// ramp across put back on the plane, and a strip narrower than the nozzle
/// keeping none of its amplitude. Before them a whole file reached 156 µm,
/// which is more than the amplitude and so more than the geometry excuses.
#[test]
fn the_surface_transform_never_rears_past_its_own_amplitude() {
    for (name, args) in SURFACE_RUNS {
        let run = run_of(name, args);
        let deepest = run.report.worst();
        let allowed = run.report.nozzle.layer * SURFACE_CREST;
        assert!(
            deepest <= allowed,
            "{name} {args:?}: a crest {:.0} um deep, over the {:.0} um its own amplitude \
             can account for\n{}",
            deepest * 1000.0,
            allowed * 1000.0,
            run.report
                .plunges
                .iter()
                .max_by(|left, right| left.depth().total_cmp(&right.depth()))
                .map_or(String::new(), |plunge| format!(
                    "  line {}: {}",
                    plunge.line,
                    plunge.text.trim()
                ))
        );
        assert!(
            run.report.under_the_bed.is_empty(),
            "{name} {args:?}: went under the plate"
        );
        assert!(
            run.report.dragged.is_empty(),
            "{name} {args:?}: {} beads dragged, longest {:.1} mm",
            run.report.dragged.len(),
            run.report
                .dragged
                .iter()
                .map(|drag| drag.span)
                .fold(0.0_f64, f64::max)
        );
    }
}

/// A travel crossing a wall this layer has left standing proud.
///
/// The slicer had no reason to lift over that wall: when it wrote the travel
/// the wall topped out at the plane the travel runs along. Raising the wall
/// afterwards is what put material in the way, so clearing it is this tool's
/// debt, not the slicer's.
#[test]
fn a_travel_never_crosses_a_bead_the_layer_has_left_standing_proud() {
    for name in [TWO_WALLS, MANY_WALLS] {
        let report = &run_of(name, &["--bricks"]).report;
        let crossings: Vec<_> = report.while_travelling().collect();
        assert!(
            crossings.is_empty(),
            "{name}: {} travels cross material left standing proud, worst {:.0} um deep\n  \
             first at line {}: {}",
            crossings.len(),
            crossings
                .iter()
                .map(|plunge| plunge.depth())
                .fold(0.0_f64, f64::max)
                * 1000.0,
            crossings[0].line,
            crossings[0].text.trim()
        );
    }
}

/// The visible wall laid at plane height beside a hidden one already standing
/// half a layer proud.
///
/// This is wall order, and it is the whole of the difference: the stock order
/// lays the visible wall last, so the loop behind it is already up when the
/// nozzle comes round. The material is 0.39 mm from the path centreline and
/// half a bead wide, which puts its edge inside the bore of the nozzle that
/// has to pass it.
#[test]
fn the_visible_wall_is_never_laid_beside_a_bead_already_standing_proud() {
    for name in [TWO_WALLS, MANY_WALLS] {
        let run = run_of(name, &["--bricks"]);
        let regions = regions(&run.gcode);
        let plowed: Vec<_> = run
            .report
            .while_extruding()
            .filter(|plunge| {
                let region = &regions[plunge.line - 1];
                region.contains("outer") || region.contains("external")
            })
            .collect();
        assert!(
            plowed.is_empty(),
            "{name}: {} beads of the visible wall are laid beside material standing up to \
             {:.0} um proud\n  first at line {}: {}",
            plowed.len(),
            plowed
                .iter()
                .map(|plunge| plunge.depth())
                .fold(0.0_f64, f64::max)
                * 1000.0,
            plowed[0].line,
            plowed[0].text.trim()
        );
    }
}

/// The same collision between two hidden walls, which is the larger half of it
/// on anything thicker than two walls and which a fix aimed at the visible
/// wall alone would leave untouched.
#[test]
fn a_hidden_wall_is_never_laid_beside_a_bead_already_standing_proud() {
    let run = run_of(MANY_WALLS, &["--bricks"]);
    let regions = regions(&run.gcode);
    let plowed: Vec<_> = run
        .report
        .while_extruding()
        .filter(|plunge| {
            let region = &regions[plunge.line - 1];
            !(region.contains("outer") || region.contains("external"))
        })
        .collect();
    assert!(
        plowed.is_empty(),
        "{} hidden-wall beads are laid beside material standing up to {:.0} um proud\n  \
         first at line {}: {}",
        plowed.len(),
        plowed
            .iter()
            .map(|plunge| plunge.depth())
            .fold(0.0_f64, f64::max)
            * 1000.0,
        plowed[0].line,
        plowed[0].text.trim()
    );
}

/// Neither order may plow the wall it prints last.
///
/// Wall order decides which loops are already standing when the nozzle comes
/// round, and the stock order — the visible wall last — is the worst case: the
/// loop behind it is up, 0.39 mm from the path centreline and half a bead
/// wide, which puts its edge inside the bore of the nozzle that has to pass
/// it.
///
/// Printing the visible wall first is not the answer on its own, and this is
/// where that shows. It is clear at two walls and not at three, because either
/// order still leaves a flat loop to be laid after a raised one somewhere in
/// the stack. What is free of it is laying every flat loop of a contour before
/// any raised one, which at two walls is the same thing as visible-wall-first
/// and above two walls is not.
#[test]
fn neither_wall_order_plows_the_wall_it_prints_last() {
    let mut complaints = Vec::new();
    for walls in [2, 3, 5] {
        for order in ["first", "last"] {
            let run = run_of(&format!("stack/{walls}/{order}"), &["--bricks"]);
            assert!(
                run.gcode.contains("corbel brick raised"),
                "{walls} walls, visible wall {order}: nothing was raised, \
                 so this fixture proves nothing"
            );
            if !run.report.is_clear() {
                complaints.push(format!(
                    "{walls} walls, visible wall printed {order}: {} moves through standing \
                     material ({} of them laying a bead), worst {:.0} um",
                    run.report.plunges.len(),
                    run.report.while_extruding().count(),
                    run.report.worst() * 1000.0
                ));
            }
        }
    }
    assert!(complaints.is_empty(), "{}", complaints.join("\n"));
}

/// Nothing may command the toolhead below the plate.
///
/// A height arrives from the slicer environment, from a container's metadata,
/// from the file's settings block and from the survey, and a number that
/// survives all four still has to describe a printer. This is the one check
/// that does not care which of them let it through.
#[test]
fn the_nozzle_is_never_taken_under_the_bed() {
    for (name, args) in EVERY_RUN {
        let report = &run_of(name, args).report;
        assert!(
            report.under_the_bed.is_empty(),
            "{name} {args:?}: {} moves go under the plate, lowest Z {:.3}\n  {}",
            report.under_the_bed.len(),
            report
                .under_the_bed
                .iter()
                .map(|(_, z, _)| *z)
                .fold(f64::INFINITY, f64::min),
            report.under_the_bed[0].2.trim()
        );
    }
}

/// A loop moved without the travel that reaches it is drawn from wherever the
/// reorder left the nozzle, and the bead it lays crosses the bed while
/// carrying the filament it was metered for a millimetre ago.
///
/// It happened: hoisting the layer's own height into the region head took the
/// travel in front of it too, and a 10-object plate came out with 28 extruding
/// moves over 60 mm, the worst 155.9 mm carrying 0.01 mm of filament. Nothing
/// else in this file could see it — it happens entirely on the layer's plane,
/// so no height is wrong anywhere.
#[test]
fn a_bead_dragged_across_the_bed_is_caught() {
    let mut rows = vec![
        "; generated by PrusaSlicer".to_owned(),
        "M83".to_owned(),
        "; layer_height = 0.2".to_owned(),
        "; nozzle_diameter = 0.4".to_owned(),
        ";LAYER_CHANGE".to_owned(),
        "G1 Z0.200 F600".to_owned(),
        ";TYPE:Perimeter".to_owned(),
        "G1 X0 Y0 F9000".to_owned(),
    ];
    for step in 1..=20 {
        rows.push(format!("G1 X{}.000 Y0.000 E0.16500", step * 5));
    }
    let honest = rows.join("\n");
    assert!(
        nozzle::inspect(&honest).dragged.is_empty(),
        "beads metered alike must not read as dragged"
    );

    rows.push("G1 X100.000 Y80.000 E0.01000".to_owned());
    let dragged = nozzle::inspect(&rows.join("\n"));
    assert_eq!(
        dragged.dragged.len(),
        1,
        "the dragged bead must be the only one caught:\n{:?}",
        dragged.dragged
    );
}
