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

## A strand is bricked bead by bead

The raise is not one answer for a strand. A strand the layer above closes over partway along is a column where the surface holds it and layer plane where it does not, and only the bead's own cells say which. `Pass::shares` writes one byte per buffered line (`Loop::column`): `1` where the layer above holds the bead, `2` where it holds nothing over it, `3` where support stands beside it, `0` where the line is not a bead. What stays loop-wide is where the strand's column ends (`over(0)`, the `CAP_SHARE` rule that was there before), the support beside it, the ramp and the phase.

Two traps, both caught by the plates' nozzle ledger:

- **A non-bead line must map to no raise.** Folding the `0` a comment gets into the same arm as a covered bead made every `; LINE_WIDTH:` and `G1 F` line of a strand "raised", so a loop with nothing raised on it read as raised: held to the end of the layer, then written flat after the raised strands beside it. On `nasty-combo` that was a 140 µm crest under a wipe.
- **The flags follow the phase.** A loop the alternation left flat — overhang-only, or phase zero — raises nothing whatever its cells say. Without the gate a loop is held as raised on its flags and written flat, which is the same plow from the other side.

`Loop::capped` is now "nothing on this loop is raised", which is what the order and the hold read.

Measured on the user's 48-layer bar (`--bricks --zaa`), against the build before this change: `Sparse infill` **+533 mm** of strand raised (L11 16.3% → 20.5%; L10/16/24 +28/+2/+27 mm), every other region identical to the millimetre, filament +0.5 mm of 8808, `invariant` 0 raised external extrusions, `nozzle` 0 faults. Pinned by `a_strand_the_surface_ends_on_is_bricked_only_where_the_surface_holds_it`, which fails with the rule put back to one answer per loop.

**A mixed strand's tail retraces its own raised arc at the flat height.** A strand raised at one end and flat at the other carries both heights on one ring, and the slicer's wipe runs back along the wall into the seam where they meet — measured on the `layer-max` plate, a wipe ending 0.06 mm from a bead standing a whole 0.140 mm proud of it. `Pass::write_loop` puts the height on the first tail move that goes somewhere, reaches it before the seam that move ends at, and never writes a `G1 Z` of its own for it. Pinned by `plates::a_layer_deeper_than_two_thirds_of_the_nozzle` and `every_awkward_setting_at_once`, both of which fail without it.

**The first bead's height is what the nozzle is at, not what the lead was meant to leave it at.** The number the beads are metered against starts from `self.nozzle_z`, so a lead whose carrier was refused for a hop the slicer made on purpose — the descent then replays as written — still puts the first bead on its own move instead of inheriting a height that never came.

## What tells you it worked

- On the 0-wall slice: 3460 loops and **1235** raised over an 8-layer window, where before it was 0 and 0 — and the outermost strand of every layer still sits exactly on its plane, which `audit.py invariant` reports as 0 of 5348 external extrusions raised.
- On a 2-wall plate with the same fill: 3523 loops, **1267** raised, against 1095 and 475 for the same plate at 15% fill. Two walls of a fourteen-object plate cannot raise 1267 loops on their own, so the count is the wall and the fill numbered as one.

## Which ring is the face of an island

An island's own outermost ring is the face it shows, so it is the anchor and it is flat. `assign_contours` marks it before the contours are built, and the test is **not** "does any other fill loop's bounding box hold this one" — on a 0-wall slice the ring around the part spans the whole part, so EVERY other island lies inside its box and none of them was marked. Measured on a user's 48-layer bar, print layers 11 and 12: two three-ring stacks of fill whose outermost rings pass 2.0 mm apart, both inside the ring around the part, both unmarked and chained into one contour. Neither could be anchored, the "widest ring in the contour" fallback went to one of them, and the other stack's face — the ring a surface band is laid against — came out **raised three phases in**, standing half a layer proud of the band beside it, with the alternation shifted for every ring behind it. That is the user's "one side causes bricks to be not staggered in the rest of the part".

The holder therefore has to be the ring **BESIDE** the loop: `outer.fill && holds(outer.outline, extent) && self.adjacent(other, index)`. On that file the rule adds 541 anchors, takes raised loops from 1178 to 1080, and splits each stack so it reads flat/raised/flat from its own face; across all 31 stored fixtures 28 are byte-identical, and the three that change (`infill-solid`, `infill-solid-1wall`, `infill-solid-2walls`) keep `invariant` at 0 and move by 11, 7 and 3 raised loops. Pinned by `a_fill_island_beside_another_is_not_numbered_from_it`, which asserts each island's face is flat and the alternation runs inward from it.
- On the `zig-zag` plate: **0 raised**, and the run fails if any loop is.
- On the other thirty-one fixtures: byte-identical output, established by building the same tree with the gate forced off and diffing every one.

## What a raised fill strand metered wrong

Bricking a fill puts raised and flat beads of the same layer side by side, a bead apart, and the slicer re-insets a concentric fill from layer to layer — so a bead written over that ground crosses the raised strands at an angle and its gap walks from a whole layer to half of one along its own length. `Ground::profile` folded each such walk into ONE piece, metered by the piece's mean, because the fold only watched the step from the running mean; a piece could therefore span a whole `level` of ground. Measured with `audit-extrusion gap` on the user's 0-wall bar, `--bricks --zaa`, against the pre-bricking build:

- `SparseInfill`: **54.11 mm** of the layer's fill path over its own gap and 7.48 mm under it, where the same file bricked nothing and read 0.00 and 0.00. Max area ratio **1.555** — a raised bead's `1.5 × 1.037`, raise times the wall flow, written over ground that is half a layer lower.
- `SolidInfill`: 5.80 mm over, 0.00 before.
- The layers that carry it are the ones where the fill changes pattern: L5, L13, L17, L23, L38, L39, L45 on that file, three-millimetre beads with `over` up to 0.32.

The fold now also cuts a piece whose own span of ground would exceed **half** the level (`Ground::merge`), which halves the walk the mean can be out by. On the same file that is **0.09 mm** over and 0.10 mm under, `SolidInfill` 0.085 mm, for **+7.9%** bead moves and 1.14% of them under 50 µm against the input's 0.04% — the move profile `SPLIT_SHARE` itself was chosen for. Pinned by `brick::ground::tests::a_walking_ground_is_written_in_pieces_that_stay_on_it` (fails with the bound at `level`) and `a_walk_narrower_than_a_step_is_left_whole`.

**A folded piece must be told how far it may walk, not only where it steps.** The first attempt at the fill bricking's metering was the threshold `SEAM_SHARE`, and this is the same mistake one level down: what a piece is metered by is a MEAN, so what bounds its error is the ground's RANGE under it and nothing else.

## A helical hop is not a primed stop

Bambu writes the layer-boundary hop as `G3 Z5.414 I-1.217 J-.019 P1 F30000` — a helix, naming only Z and an arc centre. `nozzle::ledger` read every Z-only line with a full nozzle as "a height change written as a move of its own", which that is not: the nozzle sweeps through an arc. Every layer boundary of every Bambu file counted, so the fault read `3 against 1` on a file whose pass had added none of them, and the input's own count of 1 was the same misreading. The rule now excludes a line naming an arc centre (`I`/`J`/`R`) and still counts a bare `G1 Z` with a full nozzle, which is the defect it was written for; pinned by `nozzle::tests::a_helical_hop_is_not_a_primed_stop_but_a_bare_height_is`. The pass's own charge accounting is still what decides whether the nozzle is empty when it reaches one.


Built and reverted. Every step above was written and two things came out of it that changed the shape of the work.

**`with_the_wall` is only half the gate.** Widening it to admit a fill predicate compiles, raises nothing, and the file still reports 0 loops: a loop is only OPENED where the buffer is filled, in `feed_line`. That is where infill had to be admitted, and it was not reached.

**Widening the wall's buffer is wrong anyway.** With the gate half-open the suite went from 623 green to 5 failures, all in `tests/plates.rs`, all on fixtures that have walls AND infill: granting infill a place in the same buffer changes files that lay both, because a buffered loop is reordered and re-metered wherever it sits. The answer was not to widen `continues` for every infill region but to gate the whole thing on the file stating a solid fill, so no sparse-filled file is touched at all.


## The held loops are no longer a second tour of every feature

`flush` holds every raised loop back and `write_held` lays them at the end of the layer. The wait is load-bearing — an island's walls and its infill interleave, so a loop released at the next region still has infill laid over it — but ordering the whole batch by `rise_of` alone made the held pass a second tour of every feature, each displaced lead a journey with a retract and a prime.

Measured on the two private slices this was found on, `--bricks` only, whose two islands stand 72 to 83 mm apart (one 1000-wall slice, one 0-wall at 100% concentric fill; journeys across the gap, and the distance they cover):

| | slicer's own | ordered by rise alone | ordered by stack |
|---|---|---|---|
| 1000 walls | 28 / 3.51 m | 51 / 6.28 m | 33 / 3.92 m |
| concentric fill | 27 / 3.15 m | 51 / 6.47 m | 29 / 3.09 m |

The height is owed only BETWEEN loops that run beside each other, so the wait is now ordered in two steps: `Pass::held_stacks` groups the held loops into stacks — a contour, or several whose extents come within `MAX_LOOP_GAP`, which is five times the nozzle's own reach — and `write_held` visits them from wherever the nozzle stands, lowest first within each stack. Nothing a stack owes another is an order, so any of them may be written next, and taking one half a plate away leaves the beads under the nozzle for a journey back.

**Every flat loop is still written before any raised one.** That rule is what keeps `tests/collision.rs` green, and this changes only the order among the held loops, never which loops are held. Pinned by `brick::tests::two_islands_a_plate_apart_are_not_toured_twice_a_layer`: journeys over 20 mm on a synthetic two-island file are 15 against the slicer's 9 with one queue, and 10 with stacks.

**A reordered wait can leave the nozzle standing on a raise, and the slicer's own descent then drags it through one.** A lead's descent was written for a nozzle standing where it hopped from; after the reorder it can be standing on a raised bead of the same layer instead. Measured on the `layer-max` plate: a travel plus its loop's descent, 4 um under beads standing 20 um higher, once the ordering changed. A line that would take the nozzle below what this pass has already raised in this layer, while the lead crosses it, now carries that height instead and the loop's own raise or descent settles it once the lead is over. Pinned by `plates::a_layer_deeper_than_two_thirds_of_the_nozzle`, which fails without it.

**A strand the surface holds over only part of its length is ordered by its FLAT beads.** On layer 14 of that slice the layer above's strands cross this one's instead of nesting over them, so the per-bead coverage answer alternates along a strand and the strand comes out with beads at two heights. Sorted by `rise_of`, such a strand is written as though it were raised end to end — and its flat beads then go down beside a neighbour that is already standing proud: **104 beads laid 117 um under material within the nozzle's reach**, against none in the file the slicer wrote. `Loop::grounded` and `Pass::lowest_of` sort by the lowest bead a strand really lays, which takes that to **0** with every raised loop kept (1103).

Flattening the short raised runs was the first attempt and it is wrong twice over: the plow is BETWEEN two strands, not within one, so it stayed at 104 — and it cost 57 of the 1103 raised loops. The shape looks like the one `zaa` answers with `follow_notches` and `unjab`; it is not, and a fix aimed there removes interlock without touching the defect.
