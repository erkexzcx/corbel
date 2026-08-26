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

## Driving it

```sh
flatpak run com.bambulab.BambuStudio --slice 0 --outputdir <DIR> <FILE>.3mf
```

About 2.3 s per slice for a fourteen-object plate. No display needed.

- **`--outputdir` must be inside `$HOME`.** The flatpak has its own `/tmp`, so
  a path there is written into the sandbox and silently lost — the run still
  exits 0 and prints `the parent path ... is not there, create it!`.
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
  strings before the update.
- `--skip-objects` did **not** take effect with model ids, plate ids or
  names — the validator still named the skipped objects. Treat it as
  unavailable and use a different 3mf when a variant needs fewer objects.

## Verify, twice

```sh
grep -m1 '^; wall_sequence' plate_1.gcode     # did the setting reach the file?
```

Compare the whole header against the baseline's, key by key, and print
anything that did not move. Then run the binary over each and compare what it
reports — loops, raised, surface moves — plus a hash of the output. Two
variants with the same hash are one variant.

Known refusals on the current plate, none of them worth chasing:

| setting | why |
|---|---|
| `ironing_type` | will not take at any of its four values |
| `print_sequence = by object` | objects are too tall and too close; exit -63 |
| `spiral_mode` | refuses more than one object; exit -51 |
| `enable_support` | supports make two objects' paths conflict; exit -101 |

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
5. **`layer_height`** — with a 0.8 nozzle, 0.16 and 0.56 sit at opposite ends
   of the flow model.

Then: `enable_arc_fitting` (G2/G3 at all), `seam_position` (random stresses
contour grouping), `has_scarf_joint_seam` (Z ramps *inside* a wall loop),
`fuzzy_skin` (thousands of micro-segments on the visible wall),
`filter_out_gap_fill`, `enable_prime_tower`, `detect_thin_wall`.

Not reachable from this plate but still missing from the suite: PrusaSlicer's
`;TYPE:` dialect with absolute `E`, Cura's `;LAYER:`, and a multi-filament
plate with tool changes mid-layer.

## Sizing what you store

Whole plates are 38 MB gzipped for twenty-two variants, and `--bricks --zaa`
over one is **34 s against a debug build** — the suite cannot afford it. Store
a window instead: the file's own head, then a contiguous band of layers.

- Keep the head. It carries the settings block, the start G-code and the
  machine's own nozzle wipe, which parks **below the bed on purpose** (Bambu
  wipes on a steel lip at Z-1.5). A test that demands nothing goes under the
  bed will fail on every real plate; demand instead that the output is no
  worse than the input.
- Take the band from the **middle**, not the start. The first layers carry
  every object at its widest, so layers 0-60 are 80% of the file while a
  24-layer middle band is 9.6 MB across twenty-one variants.
- Check the window still reaches both transforms. Layers 30-54 of this plate
  give 669 raised loops and 1312 surface moves; a window that raises nothing
  proves nothing.

## Storage

Fixtures are committed as ordinary files, compressed with **zstd at level 22
and a capped window**:

```sh
zstd --ultra -22 --long=23 -o <name>.gcode.zst <name>.gcode
```

`--long=23` is not optional. Without it level 22 records a **128 MB** window,
which `ruzstd` refuses (its cap is 100 MB) — and it buys **13 bytes** on a
1.3 MB file, because the window can never exceed the input anyway. Capped, the
whole set is 5.9 MB against 9.8 MB gzipped, so no LFS is needed.

Compression is slow and decompression is not, which is the right trade for a
file written once and read on every test run. The tests decode with `ruzstd`, a
pure-Rust decoder held as a dev-dependency in place of `flate2`.
