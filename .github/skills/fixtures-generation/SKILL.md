---
name: fixtures-generation
description: Generate corbel's sliced G-code fixtures headlessly from a Bambu Studio 3mf — how to drive the flatpak CLI, the traps that make a variant silently not take, which settings actually reach corbel's code, and how to size a fixture the suite can afford. Use when adding or regenerating anything under tests/fixtures/, when a fixture needs to cover a slicer setting it does not yet, or before trusting that a generated variant differs from its baseline.
---

# Generating fixtures with Bambu Studio

corbel is a post-processor, so every real defect it has ever had came from a
file a slicer actually wrote. Hand-written fixtures have no wipes, no seam
gaps, no arcs, no gap fill and no Z-hops, and they are written in whatever wall
order the author assumed. This is how to make real ones without a GUI.

## The one rule

**A variant that does not differ is worse than no variant.** It reports a pass
nobody earned. Verify every generated file twice — that the setting reached the
G-code header, and that corbel then behaves differently on it — before storing
it. Both checks are below, and both have caught silent failures here.

## The generator

`tests/fixtures/generator/generate.py` is the whole pipeline, and the plate it
slices — `tests/fixtures/generator/manyparts.3mf` — sits beside it. It patches
the plate's own settings, slices it once per setting, cuts each slice down to
its own window, checks the result twice, and stores it with zstd. **The slicer
is its first argument**, split on whitespace, so a flatpak and a binary from a
package manager are both just a string:

```sh
tests/fixtures/generator/generate.py "flatpak run com.bambulab.BambuStudio"   # flatpak
tests/fixtures/generator/generate.py /usr/bin/bambu-studio                    # package manager
tests/fixtures/generator/generate.py "flatpak run com.bambulab.BambuStudio" --variant all
tests/fixtures/generator/generate.py "flatpak run com.bambulab.BambuStudio" --variant arachne,walls-6 --check
tests/fixtures/generator/generate.py "flatpak run com.bambulab.BambuStudio" \
    --set wall_sequence="outer wall/inner wall" --name outer-first
tests/fixtures/generator/generate.py --list                                   # the variant table
```

- A plain run rewrites exactly the twenty-eight files `tests/plates.rs` reads,
  and leaves nothing untracked behind. `--variant all` adds the variants the
  suite does not read yet (`support-slim`, `infill-gyroid`).
- A variant carries four things: the settings patch, any flags the SLICER needs
  (`support` is `--arrange 1`), how many layers to store where the default window
  is too much, and `expects` where the point of the variant is that the binary
  leaves it ALONE. A plate covered in support is 2.3 MB in sixteen layers and the
  suite runs it three times, so those two variants are eight; the four
  `infill-solid*` ones are eight for the same reason.
- `--check` does everything but write, so a new slicer version can be tried
  without touching the fixtures. `--force` is not a thing: the slicer is not
  reproducible (below), so a regeneration is never byte-identical and the sizes
  are printed instead.
- `--reuse` keeps the slices in `--work` (1.2 GB with the whole matrix, and
  safe to delete at any time), which turns a re-run after a window change from
  a minute into a second.
- The fixtures are named after the **variant**, never after the plate:
  `baseline.gcode.zst`, `arachne.gcode.zst`, and so on.
- `--set` needs `--name`: an ad-hoc patch names its own fixture, and a name
  nobody chose does not belong in `tests/fixtures`.

## Driving it

```sh
flatpak run com.bambulab.BambuStudio --slice 0 --outputdir <DIR> <FILE>.3mf
```

About 4 s per slice for a fourteen-object plate, and the script runs eight at a
time. No display needed.

- **`--outputdir` must be inside `$HOME`.** The flatpak has its own `/tmp`, so a
  path there is written into the sandbox and silently lost — the run still
  exits 0 and prints `the parent path ... is not there, create it!`.
- **The slicer also writes a `result.json` into `--outputdir`**, beside the
  G-code: a stub with zeroed counters while it runs, the real slice result when
  it finishes. Point `--outputdir` at a scratch directory and never at the
  project — a run started from the repository root with `--outputdir .` leaves
  one there, which is all it takes for `git status` to show a file nobody
  asked for. The generator slices into `--work` (default
  `~/.cache/corbel-fixtures`) and never touches the project.
- **Two slices at once need a temp directory each, or one dies of SIGSEGV.**
  Bambu Studio works in `<tmp>/bamboo_model/<date>/<hour_min_sec>#2#3/`, so two
  runs beginning in the same second share one; the second reads the first's
  half-written config and exits **139** with nothing but
  `parse error ... unexpected end of input`. A plain binary takes `TMPDIR`; a
  flatpak does not see the caller's environment and keeps its own `/tmp`, so
  the variable has to be carried into the sandbox, and `--env=` has to come
  before the application id:

  ```sh
  flatpak run --env=TMPDIR=/home/me/scratch/a com.bambulab.BambuStudio --slice 0 ...
  ```
- **`--load-settings` will not take a `--export-settings` dump.** That file
  carries `"from": "project"` and the loader refuses it with
  `from project unsupported ... return -5`. Do not try to repair the header.

## Changing a setting: edit the 3mf, not a preset

A Bambu 3mf is a zip whose `Metadata/project_settings.config` is the whole
process+machine+filament config as JSON. Patch that, rezip, slice. This
sidesteps preset plumbing entirely and cannot drift from the project.

```python
cfg = json.load(open("Metadata/project_settings.config"))
cfg.update(patch)          # see the coercion rule below
# rezip the extracted tree, substituting that one member, then slice the zip
```

- **Every value in that file is a STRING**, including numbers and booleans:
  `"wall_loops": "2"`, `"layer_height": "0.4"`, `"enable_support": "0"`, and
  list-valued ones are lists of strings. Write a real int or float and Bambu
  Studio **silently falls back to the profile default** — no warning, exit 0.
  Measured: `wall_loops` 1/3/6 all produced the baseline's own 2, and
  `layer_height` 0.16 and 0.56 both came out as 0.2. Coerce everything to
  strings before the update; `generate.py` refuses anything else outright.
- **A value that is not one of the option's strings is taken just as quietly.**
  `wall_sequence` has no `"inner wall/outer wall/inner wall"` — the three-loop
  order is spelled **`inner-outer-inner wall`** — and the wrong one sliced
  exactly as before while the file went on saying `inner wall/outer wall`. The
  strings are in the binary (`strings .../bin/bambu-studio | grep -x 'inner-outer-inner
  wall'`), not in the docs.
- `--skip-objects` did **not** take effect with model ids, plate ids or
  names — the validator still named the skipped objects. Treat it as
  unavailable and use a different 3mf when a variant needs fewer objects.

## Verify, twice

```sh
grep -m1 '^; wall_sequence' plate_1.gcode     # did the setting reach the file?
```

The first check is the file's own settings block, which is exact: every
`; key = value` line the slicer writes into its head. A key still reading what
the baseline read did not take, whatever the exit code said.

The second check is **corbel's own numbers over the window** — loops, raises,
filament — and it cannot be a hash of the output. **The slicer does not
reproduce itself:** two slices of the same untouched plate differ by about
0.05% of their moves, because the infill and thin-wall regions come out in a
different thread's order (`Floating vertical shell` regions moved, 450207 G1
lines against 449993 on one pair). So `generate.py` slices the baseline twice
and uses the distance between those two as the floor a variant has to clear.
Measured over such a pair: the same 1095 loops, the same 475 raises, and
6660.9 mm of filament against 6660.2 — the counts hold still even though the
files do not.

A variant that changes the settings block and nothing in corbel's counts is
printed as `settings only`, which is worth knowing: on this plate
`enable_prime_tower` is one, because Bambu builds a tower only for a plate that
changes filament.

**`expects="declined"` inverts the check**, and it is the only way to store a
fixture whose point is that the transform does not reach it. The run then fails
if a loop IS raised on it, and the counts are not compared with the baseline at
all — a plate whose fill is its whole body has no perimeter region to count, so
"moved off the baseline" means nothing there. `infill-solid-zigzag` is that
case: a solid fill whose strands are one serpentine per island and are rotated
between layers, which must be left as sliced.

Known refusals on this plate, none of them worth chasing:

| setting | why |
|---|---|
| `ironing_type` | will not take at any of its four values |
| `print_sequence = by object` | objects are too tall and too close; exit -63 |
| `spiral_mode` | refuses more than one object; exit -51 |

Three more, and what each one taught:

- **`enable_support` looks like a refusal and is not.** As saved, it exits -101
  with `gcode path conflicts found between Cube and 3DBenchy` — the objects sit
  close enough that their tree supports run into each other. `--arrange 1`
  re-lays the plate out and it slices: 634 `; FEATURE: Support` regions and 52
  of `Support interface` on the whole plate, from this plate's own
  `support_type` of `tree(auto)`. **`--no-check` does not help** — the conflict
  is not the validator objecting to correct G-code, it is the slicer unable to
  lay the support at all, and `--no-check` exits -101 exactly as before. This
  is why a variant carries `args`: `support` is `{"enable_support": "1"}` plus
  `--arrange 1`.
- **Arranging can spill onto a second plate.** The support variants come out as
  `plate_1.gcode` (the objects) and a small `plate_2.gcode` (what would not
  fit). The script takes the first and says so on stderr; the window is taken
  from that plate alone.
- **A machine with two nozzles is not reachable from here.** Pointing
  `printer_settings_id` and `printer_model` at an H2D, doubling the
  `filament_*` lists and giving half the objects `extruder=2` fails with
  `process not compatible with printer`, exit -17: the process profile is the
  P1S one, and a plate only slices against a process written for its machine.
  A tool-change fixture needs a dual-nozzle *project*, not a patch to this one.
- **Absolute `E` is not reachable either, and it is the quiet kind.**
  `use_relative_e_distances = 0` is taken and echoed back in the settings block
  — so the header check passes — while the G-code comes out with `M83` and
  relative filament exactly as before. It would be stored as `settings only`,
  which is the flag to look for when a variant's whole point is a word in the
  G-code. A `M82` fixture has to come from a slicer that writes one.

## What is worth varying

Ranked by how much of corbel's code it reaches, not by how exotic it sounds.

1. **`wall_sequence`** — inner/outer, outer/inner, inner-outer-inner. Decides
   which loop is visible and how contours group. This is where the reported
   collision defects lived.
2. **`wall_generator`** — `arachne` gives variable-width beads, gap fill and
   thin walls; `classic` gives none of it.
3. **`wall_loops`** — 1 (nothing behind the visible wall), 3 (odd), 6 (deep).
   Alternation parity and contour renumbering.
4. **`z_hop` / `z_hop_types`** — the richest trap in the whole list. `Auto
   Lift` and `Spiral Lift` emit a **helical hop as `G2`/`G3` naming `Z` and no
   `X`/`Y`**, and `Line::is_move()` is `G0`/`G1` alone, so the hop was
   invisible to `brick` and the reordered wall was laid a whole layer above its
   plane. A hop also strands the descent that follows it.
5. **`layer_height`** — 0.08 and 0.28 sit at opposite ends of the flow model
   on a 0.4 mm nozzle, and the thin end is where the slicer starts writing a
   stationary `G1 X.. Y.. E0` at the seam of every outer wall.

Then: `enable_arc_fitting` (G2/G3 at all), `seam_position` (random stresses
contour grouping), `has_scarf_joint_seam` (Z ramps *inside* a wall loop),
`fuzzy_skin` (thousands of micro-segments on the visible wall),
`filter_out_gap_fill`, `enable_prime_tower`, `detect_thin_wall`.

6. **`enable_support`** — the one region nothing else in the suite carries, and
   the one nothing here may touch: printed to be broken off, one bead wide,
   tall and unbraced, with no wall to raise and nothing above a step to meet.
   `tree(auto)` (the plate's own) and `tree_slim` put support *beside* a wall,
   which is what `mark_columns` dilates for; `normal(auto)` in `grid` puts it
   *under* the walls, where nearly every loop is capped — on an eight-layer
   window, 211 loops and **0 raised**, against 365 and 122 for the tree shape.
   Both are worth having: they are the two halves of the same rule.
7. **`sparse_infill_pattern`** — `gyroid` against `grid`: infill that never
   repeats at a fixed pitch, which is where a footprint walk meets a path it
   cannot predict.
8. **`sparse_infill_density` 100% with `wall_loops` 0, 1 and 2** — a solid
   fill, which is a wall in everything but its label: its strands run beside
   each other and the outermost of them is the part's visible face. The four
   `infill-solid*` variants are this. **At full density Bambu Studio will lay
   only `concentric` and `zig-zag`** — asked for `grid`, `line`, `rectilinear`,
   `aligned_rectilinear` or `crosshatch` it exits 238 with
   `sparse_infill_pattern: <pattern> doesn't work at 100% density`, and
   `monotonic`/`monotonicline` are not values of the option at all. Of the two
   that work, only `concentric` stacks: a layer of it is 455 closed rings,
   against 46 open runs for `zig-zag`, which is also rotated 90° every layer.
   Patching `wall_loops` to a value the plate already has is reported as
   `SETTING DID NOT TAKE` — the check compares against the baseline's own
   settings block — so the two-wall variant patches only the fill.

Not reachable from this plate but still missing from the suite: PrusaSlicer's
`;TYPE:` dialect with absolute `E`, Cura's `;LAYER:`, and a multi-filament
plate with tool changes mid-layer.

## Sizing what you store

A whole plate is 14.9 MB of G-code here (38 MB on the 0.8 mm plate this set
replaced), and `--bricks --zaa` over one is **34 s against a debug build** —
the suite cannot afford it. Store
a window instead: the file's own head, then a contiguous band of layers.

- Keep the head. It carries the settings block, the start G-code and the
  machine's own nozzle wipe, which parks **below the bed on purpose** (Bambu
  wipes on a steel lip at Z-1.5). A test that demands nothing goes under the
  bed will fail on every real plate; demand instead that the output is no
  worse than the input.
- Take the band from the **middle**, not the start. The first layers carry
  every object at its widest, so layers 0-60 are a fifth of the file while a
  16-layer middle band is 1.3 MB of it. The stored set is layers **60 to 75**.
- Check the window still reaches both transforms. That band gives 788 loops,
  325 of them raised, and 452 surface moves on the baseline, and "a plate that
  does not reach the feature under test is worse than none" applies to a window
  as much as to a plate. `--layers` is the knob; the suite runs this one in
  12 s. A plate whose fill is solid is denser per layer — half as much G-code
  again — and its four variants are eight layers each, which costs 1.2 s a run
  over 1.4 MB.

## Storage

Fixtures are committed as ordinary files, compressed with **zstd at level 22
and a capped window**:

```sh
zstd --ultra -22 --long=23 -o <name>.gcode.zst <name>.gcode
```

`--long=23` is not optional. Without it level 22 records a **128 MB** window,
which `ruzstd` refuses (its cap is 100 MB) — and it buys **13 bytes** on a
1.3 MB file, because the window can never exceed the input anyway. Capped, the
thirty-one stored files come to **7.9 MB against 51 MB of G-code**, so no LFS
is needed. Round-trip the result — `zstd -dc` and compare — which `generate.py`
does before it writes anything.

Compression is slow and decompression is not, which is the right trade for a
file written once and read on every test run. The tests decode with `ruzstd`, a
pure-Rust decoder held as a dev-dependency in place of `flate2`.
