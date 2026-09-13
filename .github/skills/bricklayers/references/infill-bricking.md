# Bricking infill

A slicer run with `wall_loops = 0` and 100% concentric infill produces a stack of nested rings with no perimeter region in it at all: the same geometry a 1000-wall slice has, without the mid-object `Top surface` regions the wall route makes the slicer invent. `--bricks` does nothing on it. This is the design for making it work, with the measurements behind each step. Nothing here is implemented yet.

## What the file actually contains

Measured on a 0-wall, 100% concentric H2D slice of a bar with lugs, 48 layers, 4.0 MB:

- **No perimeter region, of either dialect.** `grep -o '; FEATURE: [A-Za-z ]*' | sort | uniq -c` gives `Top surface 46`, `Floating vertical shell 44`, `Sparse infill 39`, `Internal solid infill 16`, `Brim 1`, `Bottom surface 1`, `Custom 2`. Not one `Inner wall` or `Outer wall`.
- **`--bricks` reports 0 perimeter loops, 0 raised**, and its output is byte-identical to the input. That is the current behaviour and it is correct for what the code knows.
- **The nested rings live under `; FEATURE: Sparse infill`.** At 100% density with the concentric pattern, Bambu still labels the region `Sparse infill` — a label carrying "infill"/"sparse" — so `Feature::from_marker` returns `Feature::SparseInfill`. Counting paths with travel-aware grouping on layer 21: 80 paths, 23 of them closing within 0.5 mm of their start, 1294 beads, mean extent 5.87 mm, about 16 beads a ring. So the rings are there and they are ring-shaped, most of them joined to the next by a step-over rather than every one being a separate closed path.
- **Nothing else in the file is a nested stack.** `Floating vertical shell` and `Internal solid infill` are concentric fills of a solid area, not a stack of rings around a boundary; `Top surface` and `Bottom surface` are the faces.

## Why it does nothing today

`brick::Pass` buffers a loop only where `feature.is_perimeter() || is_filler(feature)`, and `is_filler` is exactly `GapFill | ThinWall`. Infill is never buffered, so no contour forms, no place in the alternation is taken, and nothing is raised.

That exclusion is deliberate and documented in `src/gcode/feature.rs`: a region read as a wall "joins the alternation, is raised, and takes the wall's flow multiplier instead of a metered gap". `Floating vertical shell` is called out by name. So this is a design change to a decision that was made on purpose, and the case for it is the one above: a concentric stack is geometrically a wall stack, and the label is the only thing that says otherwise.

## What must NOT be raised

- **A grid or rectilinear infill at any density.** Raising a line of a 15% grid stands a ridge half a layer proud with nothing above it at the same place — the next layer's lines cross it at 90°, so the ridge is exposed over almost all of its length and the nozzle comes back through it. The wall route works because a raised bead has its own stack above it; a grid line does not.
- **The outermost ring of a wall-less part.** It is the part's visible face, so a raise on it is a step on the outside. It is the anchor, and the alternation runs inward from it.
- **A lone infill loop**, unlike a lone wall contour, which `number_loops` raises today on the grounds that an inner wall always has the visible wall beside it.

The test that separates the sound case from the unsound one is nesting, and the machinery to make it already exists: `assign_contours` groups by adjacency and `number_loops`'s `ranked` branch already measures the spacing between a contour's loops where the print order is not monotonic. A concentric stack produces a contour of many loops at a consistent bead spacing; a grid produces no contour of two at all.

## The change, in the order it has to be made

1. **A second footprint in `Survey`, not a wider first one.** `Survey.here` holds the cells internal perimeters run through, and `uncovered`/`unsupported` are the difference between it and the layers either side. Adding infill to `here` would be the smallest edit and is wrong: those sets decide where a column is CAPPED, and material in them counts as covering a raise. Infill covering a wall would stop that wall being capped, which is the defect the cap rule exists to prevent. So infill needs its own cells and its own `uncovered`/`unsupported` pair, and `mark_columns` must test a loop against the set that matches its kind. Done this way every existing fixture's output stays byte-identical, because no wall reads the new set and no file with walls puts anything in it.
2. **Buffer infill loops**, in `with_the_wall`, but only the features that can form a stack — and see step 3 for the guard that keeps a grid out even if the list is generous.
3. **Raise a contour of infill only where its loops nest.** At least two loops, each within the adjacency distance of the next, at a consistent spacing. A grid fails this on every count; a concentric stack passes it. This is the safety property, and it is worth a fixture that is a grid infill and asserts byte-identical output.
4. **Anchor an infill contour by its longest loop.** There is no `Outer wall` marker on infill, so `Loop.external` is never set and `number_loops` falls back to numbering from the far end. The longest path in a nested contour is the outer ring, which is the face of the part and must stay flat; that is the anchor.
5. **Fixture.** The user's file cannot go in the repo. A 0-wall, 100% concentric slice of a shallow part has to be produced with the slicer, per the fixtures skill — and it needs a companion grid-infill slice at the same settings to pin step 3.

## What tells you it worked

- On the 0-wall slice: perimeter loops stop being 0 and the raised count is in the hundreds; the outermost ring of every layer is not raised; the top surface's beads are metered for the rings under them.
- On every existing fixture: output byte-identical, both modes, and `audit.py invariant` still 0 external perimeters raised.

## What the first attempt at this measured

Built and reverted. Every step above was written — the predicate on `Feature`, the survey's second footprint with its own draw arm, close, accessors and teardown, the cap-set selection in `mark_columns`, the anchor from the longest ring, the nesting guard in `hold_overhangs` — and two things came out of it that change the shape of the work.

**`with_the_wall` is only half the gate.** Widening it to admit `is_concentric_fill` compiles, raises nothing, and the file still reports 0 loops: a loop is only OPENED under a condition somewhere else, in the caller of the function at `src/brick.rs` around line 1755 that pushes the `Loop`. That caller is where infill has to be admitted, and it was not reached.

**Widening the wall's buffer is wrong anyway.** With the gate half-open the suite went from 623 green to 5 failures, all in `tests/plates.rs`, all on fixtures that have walls AND infill: granting infill a place in the same buffer changes files that lay both, because a buffered loop is reordered and re-metered wherever it sits. So the buffer, not just the guard, has to be structured differently — either infill is buffered without joining a wall's region in `continues`, or rings get a buffered stream of their own and a writer that never sees a wall's loops.

That is a design revision rather than more of the same list, and it is the thing to settle before writing any of the seven steps above.
