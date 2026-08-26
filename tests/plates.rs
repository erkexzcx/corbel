//! Every dialect one slicer can be made to speak, over one plate.
//!
//! A transform that holds on the stock profile and breaks on Arachne, or on a
//! scarf seam, or at a single wall, is broken. The rest of the suite pins
//! behaviour against fixtures written to exercise one mechanism each; this
//! file does the opposite, and asks whether the binary survives real output it
//! was never tuned against.
//!
//! Each fixture is the same fourteen-object plate sliced by Bambu Studio with
//! exactly one setting moved off `baseline`, and the setting is in the name.
//! They were verified to differ before being stored: a fixture that does not
//! reach the feature it is named for is worse than no fixture, because it
//! reports a pass nobody earned. See `tests/fixtures/SOURCE.txt`.

mod nozzle;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{env, fs};

const BIN: &str = env!("CARGO_BIN_EXE_corbel");

/// How deep a crest the surface transform may leave, as a share of the layer.
///
/// Its own amplitude, for the reason README.md records: two passes of a
/// followed surface can cross in height along their own length, so no ordering
/// of whole passes puts every point of the later one above the earlier.
const SURFACE_CREST: f64 = 0.5;

/// How many layers of a plate the surface transform is given.
///
/// It is the slowest thing the binary does and the grid it works on is sized
/// by the plate's span, which here is the whole bed. A quarter of the stored
/// window is enough to reach the same regions at a fifth of the time.
const FOLLOWED_LAYERS: usize = 8;

fn plate(tag: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{tag}.gcode.zst"));
    let packed = fs::read(&path).unwrap_or_else(|why| panic!("read {}: {why}", path.display()));
    let mut text = String::new();
    ruzstd::decoding::StreamingDecoder::new(packed.as_slice())
        .unwrap_or_else(|why| panic!("open {}: {why}", path.display()))
        .read_to_string(&mut text)
        .unwrap_or_else(|why| panic!("unpack {}: {why}", path.display()));
    text
}

/// The head of a plate: everything before its first layer, and `layers` of it.
fn head(gcode: &str, layers: usize) -> String {
    let marks: Vec<usize> = gcode
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("; CHANGE_LAYER"))
        .map(|(at, _)| at)
        .collect();
    let Some(&end) = marks.get(layers) else {
        return gcode.to_owned();
    };
    let kept: Vec<&str> = gcode.lines().take(end).collect();
    kept.join("\n")
}

fn processed(tag: &str, source: &str, args: &[&str]) -> (String, String) {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let room: PathBuf =
        env::temp_dir().join(format!("corbel-plate-{tag}-{}-{id}", std::process::id()));
    fs::create_dir_all(&room).expect("create sandbox");
    let input = room.join("part.gcode");
    let output = room.join("out.gcode");
    fs::write(&input, source).expect("write input");
    let run = Command::new(BIN)
        .args(args)
        .arg("--verbose")
        .arg("--output")
        .arg(&output)
        .arg(&input)
        .output()
        .expect("run binary");
    assert!(
        run.status.success(),
        "{tag} {args:?} failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let wrote = fs::read_to_string(&output).expect("read output");
    let said = String::from_utf8_lossy(&run.stderr).into_owned();
    let _ = fs::remove_dir_all(&room);
    (wrote, said)
}

/// How often each region marker is stated, so none can go missing.
fn markers(gcode: &str) -> std::collections::HashMap<String, usize> {
    let mut seen = std::collections::HashMap::new();
    for line in gcode.lines() {
        let trimmed = line.trim();
        // Only a bare comment is a marker: the stamps this tool leaves ride
        // the moves it rewrites, so a trailing comment is not a declaration.
        if !trimmed.starts_with(';') {
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix(";TYPE:")
            .or_else(|| trimmed.strip_prefix("; FEATURE:"))
        {
            *seen.entry(rest.trim().to_owned()).or_insert(0) += 1;
        }
    }
    seen
}

/// The two things every run of every plate must be able to say for itself: the
/// nozzle survives the file, and no region the slicer declared was lost on the
/// way through.
///
/// Both are judged against the INPUT, not against zero. A real machine's start
/// G-code parks the nozzle below the print surface on purpose — Bambu wipes it
/// on a steel lip at Z-1.5 — so a plate is not required to be clean, only to
/// come out no worse than it went in.
#[track_caller]
fn assert_sound(
    tag: &str,
    args: &[&str],
    source: &str,
    gcode: &str,
    allowance: f64,
    said: Option<&str>,
) {
    let started = nozzle::inspect(source);
    let report = nozzle::inspect(gcode);
    let mut faults: Vec<String> = Vec::new();

    if report.under_the_bed.len() > started.under_the_bed.len() {
        faults.push(format!(
            "{} moves go under the plate, against {} in the input",
            report.under_the_bed.len(),
            started.under_the_bed.len()
        ));
    }
    if report.dragged.len() > started.dragged.len() {
        faults.push(format!(
            "{} beads dragged, against {} in the input; longest {:.1} mm{}",
            report.dragged.len(),
            started.dragged.len(),
            report
                .dragged
                .iter()
                .map(|drag| drag.span)
                .fold(0.0_f64, f64::max),
            report.dragged.first().map_or(String::new(), |drag| format!(
                "\n    line {}: {}",
                drag.line,
                drag.text.trim()
            ))
        ));
    }
    let allowed = (report.nozzle.layer * allowance).max(started.worst());
    if report.worst() > allowed {
        faults.push(format!(
            "a crest {:.0} um deep, over the {:.0} um allowed{}",
            report.worst() * 1000.0,
            allowed * 1000.0,
            report
                .plunges
                .iter()
                .max_by(|left, right| left.depth().total_cmp(&right.depth()))
                .map_or(String::new(), |plunge| format!(
                    "\n    line {}: material at {:.3} while the nozzle is at {:.3}, {}",
                    plunge.line,
                    plunge.top,
                    plunge.at.2,
                    if plunge.extruding {
                        "laying a bead"
                    } else {
                        "travelling"
                    }
                ))
        ));
    }

    let stated = markers(source);
    let after = markers(gcode);
    for (region, count) in stated {
        let kept = after.get(&region).copied().unwrap_or(0);
        if kept < count {
            faults.push(format!(
                "{region:?} was declared {count} times and is declared {kept} times in the \
                 output — a region that loses its marker is printed as part of the one before it"
            ));
        }
    }

    // Nothing may be laid above the plane its own layer sits on, beyond the
    // half layer this transform declares it raises a loop by. A slicer puts
    // every bead of a layer exactly on the plane — measured 0.000 of a layer
    // across every stored plate — so anything over that is a bead drawn in
    // the air, which is what a lost Z descent looks like.
    let ceiling = report.nozzle.layer * (allowance + 0.5) + EPSILON;
    if let Some((top, plane)) = report
        .ceilings
        .iter()
        .flatten()
        .filter_map(|top| Some((top, *started.floors.get(top.layer)?)))
        .max_by(|left, right| (left.0.at - left.1).total_cmp(&(right.0.at - right.1)))
        .filter(|(top, plane)| top.at - plane > ceiling)
    {
        faults.push(format!(
            "a bead {:.0} um above its layer's plane, over the {:.0} um allowed\n    line {}: {}",
            (top.at - plane) * 1000.0,
            ceiling * 1000.0,
            top.line,
            top.text.trim()
        ));
    }

    // A wall's wipe belongs to the loop it retraces, so reordering loops has
    // to carry it along; drop one and the nozzle travels primed, which
    // strings, and the prime that answered it is left unbalanced.
    if report.retractions < started.retractions {
        faults.push(format!(
            "{} retractions, against {} in the input — {} were dropped",
            report.retractions,
            started.retractions,
            started.retractions - report.retractions
        ));
    }

    // The same path drawn on every layer, the same commands in the same
    // order, beads at the speeds the slicer chose, an extruder that never
    // winds backwards, and the filament the run says it added actually added.
    faults.extend(nozzle::faults(
        &nozzle::ledger(source),
        &nozzle::ledger(gcode),
        said,
    ));

    assert!(
        faults.is_empty(),
        "{tag} {args:?}: {} of what must survive did not\n  {}",
        faults.len(),
        faults.join("\n  ")
    );
}

/// A hair under a micron, which is finer than any slicer writes.
const EPSILON: f64 = 1e-9;

fn bricked(tag: &str) {
    let source = plate(tag);
    let (gcode, said) = processed(tag, &source, &["--bricks"]);
    // Nothing may stand proud of the plane it was metered against, so a run
    // without the surface transform has no allowance at all.
    assert_sound(tag, &["--bricks"], &source, &gcode, 0.0, Some(&said));
    let raised = gcode
        .lines()
        .filter(|line| line.contains("corbel brick raised"))
        .count();
    assert!(
        raised > 0,
        "{tag}: nothing was raised, so this plate proves nothing about bricking"
    );
}

fn followed(tag: &str) {
    let source = head(&plate(tag), FOLLOWED_LAYERS);
    let args = ["--bricks", "--zaa"];
    let (gcode, _) = processed(tag, &source, &args);
    // The surface transform re-meters what it reshapes, so what the run says
    // about its flow no longer accounts for the whole change.
    assert_sound(tag, &args, &source, &gcode, SURFACE_CREST, None);
    let moves = gcode
        .lines()
        .filter(|line| line.contains("corbel zaa"))
        .count();
    assert!(
        moves > 0,
        "{tag}: no surface was followed, so this plate proves nothing about --zaa"
    );
}

/// One test per plate rather than one loop over all of them, so the suite
/// spreads them over the cores it has and a failure names the dialect.
macro_rules! plates {
    ($($name:ident => $tag:literal,)*) => {
        $(
            #[test]
            fn $name() {
                bricked($tag);
            }
        )*
    };
}

plates! {
    the_stock_profile => "baseline",
    the_visible_wall_printed_first => "outer-first",
    a_wall_printed_from_both_sides_inward => "inner-outer-inner",
    a_single_wall_with_nothing_behind_it => "walls-1",
    an_odd_number_of_walls => "walls-3",
    six_walls_deep => "walls-6",
    variable_width_beads => "arachne",
    variable_width_beads_with_thin_walls => "arachne-thinwall",
    straight_moves_only => "no-arcs",
    a_seam_that_moves_every_layer => "seam-random",
    a_seam_pinned_to_one_side => "seam-back",
    a_layer_thinner_than_a_third_of_the_nozzle => "layer-thin",
    a_layer_deeper_than_two_thirds_of_the_nozzle => "layer-max",
    a_seam_ramped_along_the_bead => "scarf-seam",
    a_visible_wall_broken_into_noise => "fuzzy-skin",
    no_lift_between_islands => "no-zhop",
    a_square_lift_between_islands => "zhop-normal",
    a_helical_lift_between_islands => "zhop-spiral",
    no_prime_tower => "no-prime-tower",
    gap_fill_suppressed => "gapfill-off",
    every_awkward_setting_at_once => "nasty-combo",
}

/// The surface transform meets the same plates, on a shorter window.
///
/// Not all of them: it is by far the slowest thing the binary does, and what
/// varies between these fixtures is the shape of the walls, which is what
/// `--bricks` above is already asking about on every one. These four are the
/// ones whose walls are shaped differently enough to reach different code —
/// the stock profile, variable-width beads, a wall six loops deep, and the
/// plate that turns everything on at once.
macro_rules! surfaces {
    ($($name:ident => $tag:literal,)*) => {
        $(
            #[test]
            fn $name() {
                followed($tag);
            }
        )*
    };
}

surfaces! {
    a_surface_over_the_stock_profile => "baseline",
    a_surface_over_variable_width_beads => "arachne",
    a_surface_over_six_walls => "walls-6",
    a_surface_over_every_awkward_setting_at_once => "nasty-combo",
}
