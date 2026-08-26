# corbel

**A post-processor for G-code, written in Rust. Supports BrickLayers and ZAA.**

Two independent transforms in one binary, each doing something to a print your slicer will not:

| | transform | what it does |
|---|---|---|
| 🧱 | **[BrickLayers](#-bricklayers)** — `--bricks` | makes layers **interlock** instead of stacking as independent flat sheets, which is exactly where FDM prints crack |
| 🪄 | **[Z anti-aliasing](#-z-anti-aliasing)** — `--zaa` | follows the model's surface **inside** a layer, so a shallow top comes out as a ramp instead of a staircase |

Run either, or both in one pass. **You have to name at least one** — a run naming neither is refused, because this is handed your only copy of a file by a slicer that swallows everything it prints. Nothing else needs filling in: layer height, line width and flow are read from your file.

```
corbel --bricks --zaa
```

---

##  Install

**One-liner.** Downloads the latest release into `~/corbel` (`%USERPROFILE%\corbel` on Windows), checks the published SHA-256 sums, and prints the line to paste into your slicer. Run it again to update in place.

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/erkexzcx/corbel/main/deploy.sh | bash

# Windows (PowerShell)
irm https://raw.githubusercontent.com/erkexzcx/corbel/main/deploy.ps1 | iex
```

> 🛡️ **Windows: "An Application Control policy has blocked this file".** These builds are unsigned, and [Smart App Control](https://support.microsoft.com/en-us/topic/what-is-smart-app-control-285ea03d-fa88-4d56-882e-6698afdb7003) blocks anything unsigned. Turn it off in **Windows Security → App & browser control → Smart App Control settings**.

**By hand.** Take your platform's file from the [latest release](https://github.com/erkexzcx/corbel/releases/latest) — Linux, macOS and Windows, x86-64 and arm64 — rename it to `corbel` and `chmod +x` it. **From source**, needs only [Rust](https://rustup.rs):

```sh
git clone https://github.com/erkexzcx/corbel.git
cd corbel && cargo build --release
```

---

## 🖨️ Use

One line goes into your slicer's **post-processing scripts** field: the path to the binary plus the transforms you want. The slicer appends the G-code path itself.

```
~/corbel/corbel --bricks --zaa              # both
~/corbel/corbel --bricks                    # interlock the walls only
~/corbel/corbel --zaa                       # ramp the shallow tops only
"C:\Users\you\corbel\corbel.exe" --bricks   # Windows: full path, quoted
```

On Linux and macOS `~` works in both OrcaSlicer and Bambu Studio; on Windows give the full path, quoted if any folder name contains a space. In OrcaSlicer and Bambu Studio the field is under **Process → Others → Post-processing Scripts**, in Advanced or Expert mode. Plain `.gcode` and `.bgcode` are both accepted, and the output keeps whichever came in.

> 🙈 **OrcaSlicer's preview will not show the change — and that is normal.** It draws the toolpaths as sliced, before post-processing; the exported file *is* processed, so export it and open that file back in a slicer to check. **Bambu Studio previews after post-processing**, so the change shows straight away.

You can run it yourself instead. `-o` writes a new file and leaves the input untouched; drop it and the file is rewritten in place. `-v` says whether anything happened:

```sh
corbel --bricks --zaa -v -o modified.gcode original.gcode
# corbel: 460 layers, 11390 perimeter loops, 4890 raised by 0.100 mm
# corbel: 491 more were left flat where the wall ends and something is printed over it
# corbel: 52339.5 mm filament, 17.2% of it in raised loops; a flow of 1.025 adds 0.89% to the part
# corbel: 3653 surface moves on 127 layers followed from -0.089 to +0.100 mm of their plane, written as 15597 moves
# corbel: 271.1 mm filament in those surfaces, re-metered by +3.20% for the gaps they really cross
```

Zero raised loops means the file gave bricking nothing to work with — usually a single wall, or unrecognised region markers. `no surface shallow enough to smooth` means the part has no shallow top, which a plain box genuinely does not.

### Options

```
corbel [OPTIONS] <--bricks|--zaa> <GCODE>
```

| option | |
|---|---|
| `--bricks` | 🧱 turn bricklayering on |
| `--extra-flow <PERCENT>` | 🧱 extra flow for the walls, `0` to `50` (default `5`) — see [how much flow it adds](#-how-much-flow-it-adds) |
| `--zaa` | 🪄 turn Z anti-aliasing on |
| `-o, --output <PATH>` | write here instead of overwriting the input |
| `-v, --verbose` | print a summary of what changed |
| `--force` | run anyway on a file already processed, or on one that does not read as G-code |
| `-h, --help` / `-V, --version` | the same list, and which release this is |

Z anti-aliasing has no dials: how wide a tread is worth following is a *slope* off your layer height, and how finely a surface is sampled comes from the grid it is measured on. A dial belonging to a transform you did not name is accepted and ignored, so a leftover word in a slicer field never fails a print.

> 🔐 **Rewriting a file in place gives it a new identity.** The result is written beside the target and renamed over it, which is what makes a crash leave your original intact — but a rename publishes a new inode, so the owner, any ACL or security label, and every other name hard-linked to the file stay behind with the old one. Permission bits are copied; the rest cannot be from the standard library alone, so what can be detected is warned about before writing.

---

## 🧱 BrickLayers

`--bricks`

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="img/interlock-dark.png">
  <img alt="A wall's cross-section in three steps: as sliced, every gap between beads lines up into a channel through the wall; bricked, the same gaps are staggered but open up, because half of each is out of the nozzle's reach; bricked with extra flow, the flow fills those gaps and keys into them, ending tighter than as sliced with no channel left." src="img/interlock-light.png">
</picture>

*Seen end-on: columns are perimeter loops, rows are layers, and the red line is the join between two layers — the plane an FDM part splits along.*

**The middle step is the tool doing harm. The third is why it is worth it.**

1. **As sliced.** Two beads side by side leave a small gap where their rounded edges meet, and every layer breaks at the same height, so those gaps line up into a channel running through the wall — which is where the part splits.
2. **Bricked.** Every other loop rises half a layer, so the gaps no longer line up — but they also get *bigger*, because on a flat plane the nozzle's underside presses each gap shut as it passes and over a staggered seam half of each is out of reach. On its own, this step makes the joint weaker.
3. **Bricked + extra flow.** The material the walls gain fills those gaps and keys into them, so the wall ends with **more** contact than as sliced and no aligned channel to split along.

The gaps are drawn far larger than life — on a 0.2 mm layer they are a few microns across.

**What it touches.** Every wall, and only walls — infill, bridges, gap fill and the surfaces come out exactly as sliced, so it is never a global flow bump. Two walls are enough; three or more interlocks twice as much.

- **The first layer on the bed is left exactly as sliced.** Nothing presses a bead there, so surplus spreads sideways instead of filling anything — on a Benchy it filled in the recessed nameplate, which is exactly one layer deep. A raised column climbs to its half layer over two layers rather than stepping up in one.
- **The outer wall gets the flow and is then moved inward by half the width it gains**, so the gain feeds the joint behind it and the commanded outer face stays where the slicer drew it. What it gains is `flow - 1` of its *spacing*, not of its nominal width.
- **A `G2`/`G3` arc moves with it**, keeping its centre and changing radius by the offset, with `I`/`J` restated. A loop whose arcs cannot be moved without distorting their circle is left as sliced, which on a real slice is a handful of beads in twenty thousand.

The compensation is exact on the toolpath, but plastic is looser than a coordinate — the walls behind the visible one keep their gain, and a raised bead is out of reach of the nozzle's underside, so a bricked part can come out slightly over nominal in XY. If yours does, `--extra-flow 0` leaves only the raise, every bead metered as sliced and no wall moved; beyond that, your slicer's XY size compensation trims a measured offset.

---

### 📐 How much flow it adds

**`--extra-flow` is the extra a wall takes when your layer is as thick as your nozzle.** Print thinner than that — everyone does — and you get proportionally less:

> **extra flow ≈ `--extra-flow` × (layer height ÷ nozzle diameter)**

So the default of `5` gives **+2.5%** on a 0.2 mm layer through a 0.4 mm nozzle, and about +1% on a 0.08 mm one. Both numbers are read from your file, on every layer, so an adaptive slice is metered against what it actually printed. Nozzle size barely matters on its own; what matters is how thick your layer is next to your nozzle.

**Why those two numbers.** A bead is a rectangle with a half-round bulge on each side, and two side by side leave a corner empty where the bulges meet, which the nozzle's flat underside normally squashes shut. Bricklayering lifts every other wall half a layer, putting that corner out of reach, so the extra flow fills it instead — and its size depends on the **layer height** and the **line width**, both stated in your file.

**Set it anywhere from 0 to 50.** `0` meters every bead as sliced and moves no wall, leaving only the raise. The top of the range is for sweeping a test print rather than printing with, and there is a ceiling nobody picked: a bead can be widened until its edge reaches the centre of the loop beside it, which is the bead model's own arithmetic rather than a chosen limit.

**What it costs on the whole part is small, because it is paid only on walls.** On a ten-object plate at the default, a flow of 1.025 on the walls added **+0.89%** to the part; on a part that is mostly wall it is a little over 2%. Infill, bridges, gap fill and the surfaces are metered exactly as sliced, so it is never a global flow bump.

**Where the numbers come from.** The width is read from whichever states it first — the `SLIC3R_*` configuration your slicer exports to a post-processing script, a `.bgcode` container's metadata, or the settings block appended to plain `.gcode` — with a percentage resolved against `nozzle_diameter`. Layer heights are measured from the commanded Z one layer at a time. A file stating no width, as Cura's do, falls back to a reference profile for both the flow and the inward move, and `-v` says so.

The formula is the slicer's own bead model: PrusaSlicer's [`Flow::rounded_rectangle_extrusion_spacing`](https://github.com/prusa3d/PrusaSlicer/blob/master/src/libslic3r/Flow.cpp) spaces beads at `width − height × (1 − π/4)` and meters each at `height × spacing`, which was checked against real slices rather than assumed. The default itself is a **chosen constant**, small on purpose because it is paid on every wall and sets how far the visible wall is drawn in; only how it *scales* is derived. Micro-CT work supports the direction without having been used to fit it: [Faizaan *et al.* 2025](https://doi.org/10.1038/s41598-025-87348-2) found the voids in concentric-filled PLA to be **axially connected in every reconstruction**.

---

## 🪄 Z anti-aliasing

`--zaa`

**A layer is flat and your model usually is not.** Wherever a surface is shallower than about 45° the print leaves a staircase: each tread is one layer's surface laid at one height, each riser is a full layer. This follows the surface across each tread instead, varying the height of the extrusion *within* the layer by up to half a layer either way and metering each stretch for the gap it actually crosses.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="img/contour-dark.png">
  <img alt="A shallow slope's cross-section in two steps: as sliced, every bead of a course is laid at one height and the model's surface cuts straight through them; followed, each bead sits at the height the surface really is and is metered for the gap under it, so the tops of the beads land on that line and consecutive treads join." src="img/contour-light.png">
</picture>

*Seen end-on, on a 7° slope: columns are beads, rows are layers, blue is the wall you can see, and the dashed line is where the model's surface really runs. About four beads share a tread at 0.2 mm layers, so what you get is a finer staircase — a step a quarter the size of the one it replaced.*

**Consecutive treads join.** One ends half a layer **above** its plane exactly where the next begins half a layer **below** its own, so the full-height riser between them is gone.

**It does not need your model.** Every other implementation raycasts the mesh — [GCodeZAA](https://github.com/Theaninova/GCodeZAA) wants an STL per object and its position typed in, [BambuStudio-ZAA](https://github.com/adob/BambuStudio-ZAA) works inside the slicer. This recovers the surface from the file, because **a slicer takes its cross-section through the middle of a layer**: a layer's outline is where the surface passes half a layer *below* the plane, and the next layer's where it passes half a layer *above*. A straight climb across that strip reproduces a flat slope exactly — the case that stair-steps in the first place.

**What it touches.** The top surface, the ironing over it, and the walls — which matter more than they sound: a slope steeper than about 13° leaves a tread narrower than the wall stack on it, so your slicer emits no top-surface region at all and the staircase is entirely wall. The visible wall is always followed, and only ever lowered, since a bead standing proud is out of reach of the nozzle's underside and free to bulge on the face of the part. The walls behind it are followed when this runs alone, and left alone when bricklayering runs in the same pass, because lowering a wall onto a bead that transform has raised would close a gap your slicer metered open. Infill, bridges and anything with a layer printed over it come out exactly as sliced.

**What it costs.** Each stretch is metered for its own gap, so one sitting low takes less material and one sitting high takes more; over a tread the two cancel. A cube, and a flat plate with a boss on it, are left completely alone — every face is vertical or flat, and there is nothing to follow.

A curve needs a move per bend, so the exported file grows by a few per cent on a part with shallow tops and not at all on one without them; a **straight** climb costs nothing, since your printer interpolates height along a move already.

**A minority of layers is the right answer, not a shortfall.** A staircase only shows where a layer leaves a tread wider than the bead standing on it, and on most parts that is a small share of the layers. A Benchy is a poor subject — its hull flares outward, so barely any layer leaves a tread at all. Print a shallow dome, a low cone or a wide chamfer to see the difference.

**How shallow it goes comes from your layer height.** The widest tread it will follow is the one a **1° slope** leaves — 11.5 mm at 0.2 mm layers, 4.6 mm at 0.08 mm ones. As an angle it means the same thing on every profile and moves with an adaptive slice, and the fade runs a further quarter *past* that slope, so the widest tread it follows does not end in a step of its own.

**What it will not do.** A flat top is left alone: nothing is printed above it, so there is no far edge to climb to, and lowering it would starve a surface that was correct as sliced. So is a ledge with a wall on it, whose tread looks like a slope's and is not one — the layer below tells them apart. A bore going straight down leaves no tread to follow, though one that opens upward *is* followed, so long as it is at least a bead across and its lip is no wider than that 1° slope: narrower than a bead and it cannot be told from the gap the slicer leaves between two neighbouring lines, wider than the slope and what surrounds it is not a lip but the inside of the part. And a surface curving sharply inside one tread is approximated by a straight climb, which errs toward the plane rather than away from it.

**What it leaves behind.** Following a surface *inside* a layer means the beads of that layer are no longer all at one height, and two passes of a top surface run about a bead apart — inside the nozzle's own underside. Whichever goes down second is laid against the other, so the nozzle shears a little off it. That is bounded by the amplitude — half a layer, one bead at its extreme beside one left on the plane — and every run of the test suite is held to it. It is not zero, and six ways of making it zero were tried on a real plate and measured; each was either worse or suppressed the following the transform exists to do. The reason none of them work is that two neighbouring passes *cross* in height along their own length, so no order of whole passes has every point ascending. What is bounded instead is how steeply the surface may rise: a climb is held to one layer height per bead width, a stretch of exposure too narrow to ramp across is put back on the plane, and a strip narrower than the nozzle keeps none of its amplitude.

`--zaa` **without** `--bricks` also follows the walls behind the visible one, and where one of a neighbouring pair is covered by the layer above and the other is not, the two end up half a layer apart; running the two transforms together does not, because bricklayering leaves those walls alone.

---

## ✨ Why this one

Common to both transforms:

- 🔀 **Two transforms, one pass** — run together they compose in a single read, and they own different regions of the print, so neither disturbs the other.
- 🎛️ **Nothing to fill in, nothing to check first** — line width and nozzle come from your file, layer height is measured off the print itself, and PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio and Cura share one code path.
- 📦 **One binary, any file size, any character set** — streamed rather than loaded, so a 300 MB slice costs the same memory as a small one, and read as bytes, so an object name in any encoding passes through untouched.
- 🛡️ **Your file cannot be destroyed** — written aside and moved into place; a file that does not read as G-code is refused before a byte is written, and a second run is refused rather than stacking. Every change is stamped, so `grep corbel` tells you it ran and where.

Against [GeekDetour/BrickLayers](https://github.com/GeekDetour/BrickLayers) and [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers), both Python scripts that ask you to change slicer settings first:

- 🐍 **No Python, and no numbers to keep in sync** — no interpreter, no `-layerHeight` to match your profile, no extrusion multiplier to guess.
- 🔧 **No slicer settings to change first** — arc fitting stays on, wall order is read rather than dictated, and `.bgcode` is read and written natively with thumbnails and config copied byte for byte.
- 🧵 **No stringing from the raise** — a height change rides a travel the printer was already making, instead of stopping the toolhead over a seam with a primed nozzle.
- 🧩 **Two walls are enough, and the visible wall is in on it** — a region with one internal loop is bricked against the wall you can see; that wall takes the same flow as every other and is drawn back in by half of what it gains.

Against [Theaninova/GCodeZAA](https://github.com/Theaninova/GCodeZAA), the post-processor that Z anti-aliasing started as:

- 🧊 **No STL, object name or position to type in** — the surface is recovered from the outlines the slicer already wrote, exact for a flat slope and erring toward the plane for anything else.
- 📏 **No reach or resolution to pick** — how shallow a tread must be is a slope off your layer height, and the grid comes from the size of your part, spending a fixed budget of cells so a small part is measured finely rather than cheaply.
- 🌀 **Arcs work, on any firmware** — `G2`/`G3` are resampled rather than skipped, each sampled for its own radius to within a micron; no Klipper requirement, no wall order to set first.

---

## 🙏 Credits

Neither idea started here. This is a Rust implementation of both, without their prerequisites.

**BrickLayers** — [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers), the original script and the video that started it, and [GeekDetour/BrickLayers](https://github.com/GeekDetour/BrickLayers), the fork that worked out most of what a real slice needs.

**Z anti-aliasing** — [adob/BambuStudio-ZAA](https://github.com/adob/BambuStudio-ZAA), built into a slicer fork where the mesh is on hand to raycast; [Theaninova/GCodeZAA](https://github.com/Theaninova/GCodeZAA), the standalone post-processor and the source of the seam rule that keeps the visible wall from standing proud; and [Song et al., *Anti-aliasing for fused filament deposition*](https://arxiv.org/abs/1609.03032), the paper both trace back to.

Licensed GPL-3.0-or-later.
