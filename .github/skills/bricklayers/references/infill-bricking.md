# Bricking infill

A slicer run with `wall_loops = 0` and 100% infill produces a stack of strands laid against each other with no perimeter region in it at all: the same geometry a 1000-wall slice has, without the mid-object `Top surface` regions the wall route makes the slicer invent. `--bricks` used to do nothing on it. It now bricks it, and this is what that rests on.

## What the file actually contains

Measured on a 0-wall, 100% concentric H2D slice of a bar with lugs, 48 layers, 4.0 MB:

- **No perimeter region, of either dialect.** `grep -o '; FEATURE: [A-Za-z ]*' | sort | uniq -c` gives `Top surface 46`, `Floating vertical shell 44`, `Sparse infill 39`, `Internal solid infill 16`, `Brim 1`, `Bottom surface 1`, `Custom 2`. Not one `Inner wall` or `Outer wall`.
- **`--bricks` reports 0 perimeters, 0 raised**, and its output is byte-identical to the input. That was the behaviour before this change and it is correct for what the code knew.
- **The nested rings live under `; FEATURE: Sparse infill`.** At 100% density with the concentric pattern, Bambu still labels the region `Sparse infill` — a label carrying "infill"/"sparse" — so `Feature::from_marker` returns `Feature::SparseInfill`. Counting paths with travel-aware grouping on layer 21: 80 paths, 23 of them closing within 0.5 mm of their start, 1294 beads, mean extent 5.87 mm, about 16 beads a ring.
- **Nothing else in the file is a nested stack.** `Floating vertical shell` and `Internal solid infill` are concentric fills of a solid area, not a stack of rings around a boundary; `Top surface` and `Bottom surface` are the faces. They are left out of the candidate set for exactly that reason: `Floating vertical shell` classifies as `SolidInfill`, and `SolidInfill` also carries bridges and the horizontal skins.

## Why it did nothing

`brick::Pass` buffered a loop only where `feature.is_perimeter() || is_filler(feature)`, and `is_filler` is exactly `GapFill | ThinWall`. Infill was never buffered, so no contour formed, no place in the alternation was taken, and nothing was raised.

That exclusion is deliberate and documented in `src/gcode/feature.rs`: a region read as a wall "joins the alternation, is raised, and takes the wall's flow multiplier instead of a metered gap". `Floating vertical shell` is called out by name. So this is a change to a decision that was made on purpose, and the case for it is the one above: a concentric stack is geometrically a wall stack, and the label is the only thing that says otherwise.

## What must NOT be raised

- **A fill whose strands are laid ACROSS each other.** Not "a grid", as this file first said — the rule is geometric and it is measured. Bambu will lay only `concentric` and `zig-zag` at full density (asked for `grid`, `line`, `rectilinear` or `crosshatch` it exits 238 with `doesn't work at 100% density`), and of those two `zig-zag` is one serpentine per island whose strands are joined at the ends — 46 open runs a layer whose ends are 7 to 35 mm apart, against 455 closed rings on the concentric one — and the slicer **rotates it 90° every layer** (layer 62 at 45°, layer 63 at 135°; the concentric fill runs at 90° and 0° both layers). Raising a strand there puts a ridge across the island that the next layer cuts straight through, over the whole of its length.
- **The outermost ring of a wall-less part.** It is the part's visible face, so a raise on it is a step on the outside. It is the anchor, and the alternation runs inward from it.
- **A lone infill loop**, unlike a lone wall contour, which `number_loops` raises today on the grounds that an inner wall always has the visible wall beside it. A fill can be alone in an island that holds a single ring.
- **Any infill at all on a file that does not state a solid fill.** The density is the one setting read, from the file's own block or from the `SLIC3R_*` a slicer exports; a fill at 15% lays its strands millimetres apart, and half of a spaced stack raised is a bead standing up beside nothing.

## How it is decided

Two tests, both geometric, both on data the pass already has:

1. **Is the strand a ring?** `Pass::closes_on_itself` — its path returns to where it started, within one stated bead width, the same tolerance and for the same reason `move_walls` uses it. A ring does not close exactly: the slicer stops a bead short so the two ends do not pile up at the seam.
2. **Do the rings NEST?** `outermost` over the contour's rings — one loop whose extent holds every other's. Nested strands lie one inside the next, so the strand one layer up is the same strand at the same place; strands laid across each other have no such loop. Where they do not nest, the contour's fill is demoted to `filler`: no place in the alternation and no wall flow.

The nesting test is also what names the anchor. `Loop::external` is never set on a fill loop, so `number_loops` would fall back to counting from the far end — which raises the visible face. A contour that is a stack of fill and holds no wall of its own has its outermost ring marked `external`, and the wall flow's inward move then applies to it as it does to any visible wall.

**The outermost ring of every island is marked BEFORE the contours are built.** That is the one ordering constraint in the whole change, and it is not obvious: `assign_contours`'s `taken` rule — one wall shows one visible loop, so a second one is a second wall however close it runs — is what stops two islands being numbered from one anchor, and it can only see loops already marked. A slicer marks the visible wall of a wall and never one of a fill, so two islands whose fills come within the join distance chain into ONE contour and the second island's rings are numbered from the first island's face, which raises the second island's own outer ring: the one place a raise is a step on the surface. Marking first fixes that with the rule that was already there rather than with a new one. A ring a WALL holds is not marked — an island's fill runs inside its wall and is numbered with it, which is what makes the wall and the fill touching it one stack.

Measured with the marking in place on the user's 48-layer 0-wall slice: **0 of the 1177 raised loops were a marked anchor.** Before it, whole contours of a second island were numbered from the first island's anchor.

## The change, where it went

1. **`Survey.solid_fill`**, read from `sparse_infill_density`/`fill_density` — in the file's settings block, or from the `SLIC3R_*` the slicer exports, merged before the pass begins because the survey draws the cells capping is measured against as it reads. **The pattern is deliberately not read**: what makes a fill a wall is the geometry, and the pass measures that itself.
2. **The survey draws those beads into `here`**, exactly where an internal perimeter goes, so a raised strand is capped where its column ends and dated for the object's top like a wall. Gated on the same flag, so no file that states a sparse fill has any cell of it counted — which is why every one of the thirty-one stored fixtures comes out byte for byte as it did before this change.
3. **`Pass` buffers a `SparseInfill` region as loops** where that flag is set, and `with_the_wall` keeps the buffer open across the wall/fill boundary, so a wall and the fill touching it are one contour and one alternation.
4. **`assign_contours` marks each island's outermost ring** as the visible one, before it groups anything; see above.
5. **`Pass::settle_fill_contours`** then runs between `assign_contours` and `number_loops` and applies the two tests above, demoting what fails them to `filler` so it takes no place in the alternation and no wall flow.

## What tells you it worked

- On the 0-wall slice: 3460 loops and 1224 raised over an 8-layer window, where before it was 0 and 0 — and the outermost strand of every layer still sits exactly on its plane.
- On a 2-wall plate with the same fill: 3523 loops, 1264 raised, against 1095 and 475 for the same plate at 15% fill. Two walls of a fourteen-object plate cannot raise 1264 loops on their own, so the count is the wall and the fill numbered as one.
- On the `zig-zag` plate: **0 raised**, and the run fails if any loop is.
- On the other thirty-one fixtures: byte-identical output, established by building the same tree with the gate forced off and diffing every one.

## What the first attempt at this measured

Built and reverted. Every step above was written and two things came out of it that changed the shape of the work.

**`with_the_wall` is only half the gate.** Widening it to admit a fill predicate compiles, raises nothing, and the file still reports 0 loops: a loop is only OPENED where the buffer is filled, in `feed_line`. That is where infill had to be admitted, and it was not reached.

**Widening the wall's buffer is wrong anyway.** With the gate half-open the suite went from 623 green to 5 failures, all in `tests/plates.rs`, all on fixtures that have walls AND infill: granting infill a place in the same buffer changes files that lay both, because a buffered loop is reordered and re-metered wherever it sits. The answer was not to widen `continues` for every infill region but to gate the whole thing on the file stating a solid fill, so no sparse-filled file is touched at all.

