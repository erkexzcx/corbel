#!/usr/bin/env python3
"""Slice a plate with Bambu Studio and store the result as a corbel fixture.

A fixture is a real slicer's own G-code, so the only way to add one is to run a
real slicer. This drives one headlessly, from a 3mf, with a setting moved off
the plate's own profile — and then *checks that the setting took*, because the
failure mode here is silent. Values written as numbers where Bambu Studio
stores strings are quietly dropped; a value it does not recognise is taken
without a word and the plate is sliced exactly as before. Both leave a variant
that only looks different.

So every fixture is checked twice. The file must come back naming the new
value in its own settings block, which is exact and catches the fallback; and
corbel's own counts over it must move off the baseline's, which is what says
the fixture reaches the transform at all. A hash of the output is no use for
the second check — the slicer is not reproducible, and two slices of the same
untouched plate differ by about 0.05% of their moves — so the baseline is
sliced twice and the difference between those two is the floor a variant has
to clear.

    ./generate.py "flatpak run com.bambulab.BambuStudio"
    ./generate.py /usr/bin/bambu-studio --variant arachne,walls-6
    ./generate.py "flatpak run com.bambulab.BambuStudio" --variant all    ./generate.py "flatpak run com.bambulab.BambuStudio" --naming variant --variant all    ./generate.py "flatpak run com.bambulab.BambuStudio" \\
        --set wall_sequence="outer wall/inner wall" --name manyparts-outer-first

The slicer is the first argument and is split on whitespace, so a flatpak (a
command word plus three more) and a package-manager binary are both just a
string. Anything the slicer cannot be made to do on this plate exits non-zero
and names itself, rather than being stored as a variant that never took.

See ../../../.github/skills/fixtures-generation/SKILL.md for what is worth
varying, how big a window the suite can afford, and the traps.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path
from typing import NamedTuple

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent
REPO = FIXTURES.parent.parent

# The plate this directory ships with. Any other 3mf can be named with
# --plate; no 3mf is ever chosen by preference, only by being asked for.
DEFAULT_PLATE = "manyparts.3mf"

# zstd's level and its window are not free choices: level 22 without a window
# cap records a 128 MB window, which `ruzstd` — the decoder the suite uses —
# refuses. The cap buys 13 bytes and so costs nothing that decodes.
ZSTD_LEVEL = ("--ultra", "-22", "--long=23")

# A band of layers, not a whole plate: this one is 460 layers and 14.9 MB, and
# `--bricks --zaa` over it is far more than the suite can afford. Taken from
# the middle, where every object is still present, and deep enough that the
# transforms are past their five-layer climb and have surfaces to work on.
DEFAULT_FROM = 60
DEFAULT_LAYERS = 24

# How many slicers to run at once. A slice is a subprocess that sits at one
# core for seconds at a time, so this is only bounded by the machine.
DEFAULT_JOBS = min(8, os.cpu_count() or 1)


# Every value in `Metadata/project_settings.config` is a STRING, including
# numbers and booleans and every member of a list. Write a real int or float
# and Bambu Studio silently falls back to the profile default — which is how
# six variants were once stored byte-identical to the baseline. The table
# below is written in strings on purpose, and `coerce` refuses anything else
# rather than quietly fixing it up.
class Variant(NamedTuple):
    """What a variant moves off the plate's profile, and how it is sliced."""

    patch: dict[str, object]
    args: tuple[str, ...]
    layers: int | None = None
    expects: str = ""


def v(
    patch: dict[str, object] | None = None,
    args: tuple[str, ...] = (),
    layers: int | None = None,
    expects: str = "",
) -> "Variant":
    """One variant: what to move off the plate's profile, and how to slice it.

    `args` are the slicer's own flags, for a setting whose value is not the
    whole of it. `enable_support` is the one that needs them: this plate's
    objects sit close enough that their tree supports run into each other, and
    `--arrange 1` re-lays the plate out and the slice goes through.

    `layers` is how deep a window this variant is stored as, where the default
    is too much: a plate covered in support is 2.3 MB in sixteen layers and the
    suite runs it three times.

    `expects` is what the variant is FOR, where that is not "corbel changes
    something": `"declined"` is a variant a transform must decline — the
    slicer's own settings are the case under test, and corbel's counts must
    stay exactly on the baseline's. The default is the opposite: a variant
    whose whole purpose is to reach some code has to move off the baseline in
    one of corbel's own counts, or it reports a pass nobody earned.
    """
    return Variant(patch or {}, tuple(args), layers, expects)


# Every value in `Metadata/project_settings.config` is a STRING, including
# numbers and booleans and every member of a list. Write a real int or float
# and Bambu Studio silently falls back to the profile default — which is how
# six variants were once stored byte-identical to the baseline. The table
# below is written in strings on purpose, and `coerce` refuses anything else
# rather than quietly fixing it up.
VARIANTS: dict[str, "Variant"] = {
    "baseline": v(),
    # Which loop is the visible one, and how the contours group. The three-loop
    # order is spelled `inner-outer-inner wall`; `inner wall/outer wall/inner
    # wall` is not a value of this option at all, and Bambu Studio takes it
    # without a word while slicing exactly as it did before.
    "outer-first": v({"wall_sequence": "outer wall/inner wall"}),
    "inner-outer-inner": v({"wall_sequence": "inner-outer-inner wall"}),
    # How many loops a wall has, and so where a contour boundary falls.
    "walls-1": v({"wall_loops": "1"}),
    "walls-3": v({"wall_loops": "3"}),
    "walls-6": v({"wall_loops": "6"}),
    # Variable-width beads, thin walls and gap fill, in one.
    "arachne": v({"wall_generator": "arachne"}),
    "arachne-thinwall": v({"wall_generator": "arachne", "detect_thin_wall": "1"}),
    # Arcs at all: G2/G3 rather than a fan of chords.
    "no-arcs": v({"enable_arc_fitting": "0"}),
    # Where the seam lands, which is what decides how a wall groups.
    "seam-random": v({"seam_position": "random"}),
    "seam-back": v({"seam_position": "back"}),
    # A layer thinner and deeper than the middle of the flow model.
    "layer-thin": v({"layer_height": "0.08"}),
    "layer-max": v({"layer_height": "0.28"}),
    # A Z ramp *inside* a wall loop.
    "scarf-seam": v({"has_scarf_joint_seam": "1"}),
    # Thousands of micro-segments on the visible wall.
    "fuzzy-skin": v({"fuzzy_skin": "external"}),
    # The richest trap of the lot: Auto Lift and Spiral Lift are helical hops
    # written as G2/G3 naming Z and no X/Y, which `Line::is_move()` cannot see.
    "no-zhop": v({"z_hop": ["0", "0"]}),
    "zhop-normal": v({"z_hop_types": ["Normal Lift", "Normal Lift"]}),
    "zhop-spiral": v({"z_hop_types": ["Spiral Lift", "Spiral Lift"]}),
    # The tower, and the gap fill a wall is buffered with.
    "no-prime-tower": v({"enable_prime_tower": "0"}),
    "gapfill-off": v({"filter_out_gap_fill": "1"}),
    # Support: the one region nothing else in the suite carries, and the one
    # nothing here may touch — printed to be broken off, a bead wide, tall and
    # unbraced. This plate's own support type is `tree(auto)`, which is the
    # shape both support defects were found on, and `support-slim` and
    # `support-grid` are the other two shapes a wall beside support meets.
    "support": v({"enable_support": "1"}, ["--arrange", "1"], 8),
    "support-slim": v(
        {"enable_support": "1", "support_style": "tree_slim"}, ["--arrange", "1"], 8
    ),
    "support-grid": v(
        {
            "enable_support": "1",
            "support_type": "normal(auto)",
            "support_style": "grid",
        },
        ["--arrange", "1"],
        8,
    ),
    # Infill that is not straight lines: a footprint walk meets a path that
    # never repeats at a fixed pitch.
    "infill-gyroid": v({"sparse_infill_pattern": "gyroid"}),
    # Infill laid solid, which is the one fill a stagger may run through: at
    # full density its strands are laid against each other instead of
    # millimetres apart, so they are a wall's loops in everything but their
    # label. `wall_loops: 0` leaves no perimeter region in the file at all and
    # the outermost strand IS the part's outer face, which is the case
    # `--bricks` used to do nothing on; the one- and two-wall plates put the
    # same fill inside a wall, where the two stacks touch and have to be
    # numbered as one.
    #
    # Two patterns at zero walls, because those are the only two Bambu Studio
    # will lay at full density at all: asked for `grid`, `line`, `rectilinear`,
    # `aligned_rectilinear`, `crosshatch` or anything else it exits 238 with
    # "sparse_infill_pattern: grid doesn't work at 100%% density". One of the
    # two is a stack that must be bricked and the other is not; see below.
    "infill-solid": v(
        {
            "wall_loops": "0",
            "sparse_infill_density": "100%",
            "sparse_infill_pattern": "concentric",
        },
        layers=8,
    ),
    "infill-solid-1wall": v(
        {
            "wall_loops": "1",
            "sparse_infill_density": "100%",
            "sparse_infill_pattern": "concentric",
        },
        layers=8,
    ),
    "infill-solid-2walls": v(
        {
            "sparse_infill_density": "100%",
            "sparse_infill_pattern": "concentric",
        },
        layers=8,
    ),
    # The other pattern Bambu will lay at full density, and the one that must
    # NOT be bricked. `zig-zag` is a single serpentine per island — its strands
    # are connected at the ends, so one run is 45 paths of ~35 mm rather than
    # 455 closed rings — and the slicer rotates it 90 degrees every layer:
    # measured on this plate, layer 62 runs at 45 degrees and layer 63 at 135.
    # A raised strand is therefore crossed at right angles by the layer above
    # over the whole of its length, with nothing at the same place to bond to,
    # and the nozzle comes back through the ridge. Concentric rings stack
    # instead — the same two layers run at 90 and 0 degrees, 576 and 580
    # strands. That is what the transform refuses and what this fixture pins.
    "infill-solid-zigzag": v(
        {
            "wall_loops": "0",
            "sparse_infill_density": "100%",
            "sparse_infill_pattern": "zig-zag",
        },
        layers=8,
        expects="declined",
    ),
}

# Everything awkward at once. Built from the entries above rather than
# repeated, so a variant that changes here changes there too.
VARIANTS["nasty-combo"] = v(
    {
        **VARIANTS["arachne"].patch,
        **VARIANTS["inner-outer-inner"].patch,
        "wall_loops": "3",
        **VARIANTS["no-arcs"].patch,
        **VARIANTS["seam-random"].patch,
        **VARIANTS["scarf-seam"].patch,
        **VARIANTS["zhop-spiral"].patch,
        **VARIANTS["gapfill-off"].patch,
    }
)

# The variants the suite actually reads, so a plain run reproduces exactly the
# fixtures under tests/fixtures/ and leaves nothing untracked behind it.
# `--variant all` is the whole matrix.
STORED = (
    "baseline",
    "outer-first",
    "inner-outer-inner",
    "walls-1",
    "walls-3",
    "walls-6",
    "arachne",
    "arachne-thinwall",
    "no-arcs",
    "seam-random",
    "seam-back",
    "layer-thin",
    "layer-max",
    "scarf-seam",
    "fuzzy-skin",
    "no-zhop",
    "zhop-normal",
    "zhop-spiral",
    "no-prime-tower",
    "gapfill-off",
    "nasty-combo",
    "support",
    "support-grid",
    "infill-solid",
    "infill-solid-1wall",
    "infill-solid-2walls",
    "infill-solid-zigzag",
)


def coerce(patch: dict[str, object]) -> dict[str, object]:
    """Check that a patch is what the config file stores: strings, always."""
    for key, value in patch.items():
        if isinstance(value, list):
            for member in value:
                if not isinstance(member, str):
                    raise SystemExit(f"{key}: {member!r} is not a string")
        elif not isinstance(value, str):
            raise SystemExit(
                f"{key}: {value!r} is not a string. Bambu Studio drops a number "
                f"or a boolean here without a word, and the variant then comes "
                f"out identical to the baseline."
            )
    return patch


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def fixture_name(variant: str) -> str:
    """What one fixture is called: the setting it moves, and `baseline`.

    The suite reads these by name (`tests/plates.rs`), so a plate's stock
    profile is `baseline.gcode.zst` and every variant is named after the one
    setting it changes. Nothing carries the plate's own name: a plate becomes
    *the* fixture set, and the set keeps the shape it has always had.
    """
    return "baseline.gcode.zst" if variant == "baseline" else f"{variant}.gcode.zst"


def is_layer_marker(line: bytes) -> bool:
    """A layer boundary, in either dialect this can be pointed at."""
    stripped = line.strip()
    return stripped == b";LAYER_CHANGE" or stripped.startswith(b"; CHANGE_LAYER")


def window(text: bytes, start: int, count: int) -> bytes:
    """The plate's head, then `count` layers from `start`.

    The head is not optional: it carries the settings block, the start G-code
    and the machine's own nozzle wipe, which Bambu parks *below the bed* on
    purpose. A window taken without it has no settings to compare and no
    below-the-bed wipe for a test to judge the output against.
    """
    lines = text.splitlines(keepends=True)
    marks = [i for i, line in enumerate(lines) if is_layer_marker(line)]
    if len(marks) < 2:
        raise SystemExit("this G-code states no layer boundaries, so there is nothing to window")
    if start + 1 > len(marks):
        raise SystemExit(f"asked for layer {start} of {len(marks)}")
    first = marks[start]
    last = marks[start + count] if start + count < len(marks) else len(lines)
    return b"".join(lines[: marks[0]] + lines[first:last])


def config_block(gcode: bytes) -> dict[str, str]:
    """The `; key = value` lines the slicer writes into the file's head.

    This is the file saying what it was sliced with, which is the only place a
    setting that did not take can be seen at all.
    """
    stated: dict[str, str] = {}
    for line in gcode.splitlines():
        if not line.startswith(b"; ") or b" = " not in line:
            continue
        key, _, value = line[2:].partition(b" = ")
        try:
            stated[key.decode()] = value.decode().strip()
        except UnicodeDecodeError:
            continue
    return stated


def stamp(gcode: bytes) -> str:
    """The slicer's own version, off the header block."""
    for line in gcode.splitlines()[:40]:
        if line.startswith((b"; BambuStudio", b"; OrcaSlicer", b"; PrusaSlicer")):
            return line[2:].decode(errors="replace").strip()
    return "unknown slicer"


def temp_isolation(slicer: list[str], room: Path) -> tuple[list[str], dict[str, str]]:
    """Give this slice a temp tree of its own, whatever kind of slicer it is.

    Bambu Studio works in a temp directory named from the clock —
    `<tmp>/bamboo_model/<date>/<time>#2#3/` — so two slices that start in the
    same second share one: the second reads the first's half-written config
    and dies of SIGSEGV (exit 139), saying only that a JSON file ended
    mid-object. A directory each fixes it, and then every core can be used.

    A plain binary takes it from `TMPDIR`, which it would honour anyway. A
    flatpak does not see the caller's environment at all and keeps its own
    /tmp, so the variable has to be carried *into* the sandbox with
    `--env=`, which must come before the application id.
    """
    room_tmp = room / "tmp"
    room_tmp.mkdir(parents=True, exist_ok=True)
    if "flatpak" in Path(slicer[0]).name:
        where = slicer.index("run") + 1 if "run" in slicer else 1
        return (
            [*slicer[:where], f"--env=TMPDIR={room_tmp}", *slicer[where:]],
            dict(os.environ),
        )
    return slicer, {**os.environ, "TMPDIR": str(room_tmp)}


def run_slicer(
    slicer: list[str], plate: Path, room: Path, slug: str, extra: tuple[str, ...] = ()
) -> bytes:
    """Slice one patched plate and hand back the G-code the slicer wrote."""
    command, env = temp_isolation(slicer, room)
    room.mkdir(parents=True, exist_ok=True)
    run = subprocess.run(
        [*command, "--slice", "0", *extra, "--outputdir", str(room), str(plate)],
        capture_output=True,
        text=True,
        env=env,
    )
    written = sorted(room.glob("*.gcode"))
    if run.returncode != 0 or not written:
        raise SystemExit(
            f"{slug}: the slicer wrote no G-code (exit {run.returncode})\n"
            f"{(run.stdout + run.stderr).strip()[-2000:]}"
        )
    if len(written) > 1:
        # `--arrange` puts whatever will not fit on a second plate, and a
        # support variant needs arranging. The first plate is the one with the
        # objects on it; the window is taken from that, and the rest of the
        # plate is not part of the fixture.
        print(
            f"{slug}: the slicer wrote {len(written)} plates; taking {written[0].name}",
            file=sys.stderr,
        )
    return written[0].read_bytes()


def patch_plate(plate: Path, patch: dict[str, object], into: Path) -> Path:
    """A copy of the plate with its own project settings moved.

    A Bambu 3mf is a zip whose `Metadata/project_settings.config` is the whole
    process+machine+filament config as JSON. Patching that and rezipping
    sidesteps preset plumbing entirely and cannot drift from the project —
    `--load-settings` refuses an `--export-settings` dump outright.
    """
    member = "Metadata/project_settings.config"
    into.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(plate) as zin:
        cfg = json.loads(zin.read(member))
        cfg.update(patch)
        with zipfile.ZipFile(into, "w", zipfile.ZIP_DEFLATED) as zout:
            for item in zin.infolist():
                data = zin.read(item.filename)
                if item.filename == member:
                    data = json.dumps(cfg, indent=4).encode()
                zout.writestr(item, data)
    return into


def corbel(binary: Path, gcode: bytes, room: Path) -> dict[str, object]:
    """Run the binary over a fixture and read back what it says it did.

    These numbers, not a hash of the output, are what a variant is judged on.
    The slicer is not reproducible — two slices of the same untouched plate
    differ by about 0.05% of their moves — so two runs of corbel over two
    windows of it can never agree byte for byte. What does hold still is what
    corbel counts: measured across two baseline slices, the same 1095 loops
    and 475 raises, and 6660.9 mm of filament against 6660.2.
    """
    room.mkdir(parents=True, exist_ok=True)
    source, output = room / "in.gcode", room / "out.gcode"
    source.write_bytes(gcode)
    run = subprocess.run(
        [str(binary), "--bricks", "--verbose", "--output", str(output), str(source)],
        capture_output=True,
        text=True,
    )
    if run.returncode != 0:
        raise SystemExit(f"corbel refused a fixture: {run.stderr.strip()[-2000:]}")
    numbers: dict[str, object] = {}
    for name, pattern in (
        ("loops", r"(\d+) loops"),
        ("raised", r"(\d+) raised by"),
        ("filament", r"([\d.]+) mm filament"),
    ):
        found = re.search(pattern, run.stderr)
        numbers[name] = float(found.group(1)) if found else 0.0
    # Whether the file came back byte for byte. A variant that must not be
    # touched is only proved by this: a transform that raises nothing can still
    # re-meter, re-order or re-mark a file, and every one of those is a change
    # to somebody's print.
    numbers["changed"] = output.read_bytes() != gcode
    return numbers


def compress(source: Path, target: Path) -> None:
    """Store a fixture the way the suite reads it, and prove it reads back."""
    subprocess.run(
        ["zstd", *ZSTD_LEVEL, "-q", "-f", "-o", str(target), str(source)], check=True
    )
    back = subprocess.run(["zstd", "-dc", str(target)], capture_output=True, check=True).stdout
    if back != source.read_bytes():
        raise SystemExit(f"{target}: zstd did not give back what it was given")


def write_fixture(target: Path, gcode: bytes, work: Path) -> str:
    """Store one window and say what changed.

    A regeneration is never byte-identical — the slicer is not reproducible —
    so this cannot be a check that refuses to overwrite. The sizes are printed
    instead, and a fixture that grew or shrank by more than a few percent is
    worth looking at before it is committed.
    """
    room = work / "pack"
    room.mkdir(parents=True, exist_ok=True)
    plain = room / target.name.removesuffix(".zst")
    plain.write_bytes(gcode)
    packed = room / target.name
    compress(plain, packed)
    before = target.stat().st_size if target.exists() else 0
    target.write_bytes(packed.read_bytes())
    after = target.stat().st_size
    if after == before:
        return "unchanged"
    if before == 0:
        return f"{after / 1024:.0f} KB"
    return f"{after / 1024:.0f} KB, was {before / 1024:.0f} KB"


def measure(
    slug: str,
    patch: dict[str, object],
    plate: Path,
    work: Path,
    slicer: list[str],
    binary: Path,
    base: dict[str, object] | None,
    start: int,
    layers: int,
    reuse: bool,
) -> dict[str, object]:
    """Slice, window and measure one variant. Never touches the fixtures."""
    args = VARIANTS.get(slug, VARIANTS["baseline"]).args
    sliced = work / "slices" / f"{slug}.gcode"
    if not reuse and sliced.exists():
        sliced.unlink()
    if not sliced.exists():
        patched = patch_plate(plate, patch, work / "plates" / f"{slug}.3mf")
        gcode = run_slicer(slicer, patched, work / "run" / slug, slug, args)
        sliced.parent.mkdir(parents=True, exist_ok=True)
        sliced.write_bytes(gcode)
    measured = window(sliced.read_bytes(), start, VARIANTS.get(slug, VARIANTS["baseline"]).layers or layers)
    cut = work / "windows" / f"{slug}.gcode"
    cut.parent.mkdir(parents=True, exist_ok=True)
    cut.write_bytes(measured)

    stated = config_block(measured)
    # The file naming the new value is the proof the setting reached the
    # slicer. A key that still reads what the baseline read did not take,
    # whatever the exit code said — this is the check that catches the silent
    # fallback, and it is exact. A key the block does not echo at all cannot
    # be judged this way, so it is listed and the counts decide instead.
    # Checked before anything is written, so a variant that fell back cannot
    # reach the fixtures at all.
    echoed: dict[str, str] = {}
    stuck: dict[str, str] = {}
    blind: list[str] = []
    if base is not None:
        for key, wanted in patch.items():
            now, was = stated.get(key), base["stated"].get(key)
            if now is None and was is None:
                blind.append(key)
            elif now == was:
                stuck[key] = f"{now!r}, wanted {wanted!r}"
            else:
                echoed[key] = str(now)
    return {
        "name": slug,
        "bytes": len(measured),
        "stated": stated,
        "stuck": stuck,
        "echoed": echoed,
        "blind": blind,
        "stamp": stamp(measured),
        "expects": VARIANTS.get(slug, VARIANTS["baseline"]).expects,
        **corbel(binary, measured, work / "check" / slug),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "slicer",
        nargs="?",
        help='the slicer and its own arguments, e.g. "flatpak run com.bambulab.BambuStudio"',
    )
    parser.add_argument("--plate", type=Path, default=None, help=f"the 3mf to slice (default: {DEFAULT_PLATE})")
    parser.add_argument("--variant", action="append", default=[], help="named variant, repeatable, comma-separated, or `all`")
    parser.add_argument("--set", action="append", default=[], metavar="KEY=VALUE", help="an ad-hoc setting patch, which needs --name")
    parser.add_argument("--name", default=None, help="what to call the fixture an ad-hoc --set makes")
    parser.add_argument("--from", dest="start", type=int, default=DEFAULT_FROM, help="first layer to keep")
    parser.add_argument("--layers", type=int, default=DEFAULT_LAYERS, help="how many layers to keep")
    parser.add_argument("--into", type=Path, default=FIXTURES, help="where fixtures are written")
    parser.add_argument("--work", type=Path, default=Path.home() / ".cache/corbel-fixtures")
    parser.add_argument("--corbel", type=Path, default=None, help="the binary to verify with")
    parser.add_argument("--jobs", type=int, default=DEFAULT_JOBS)
    parser.add_argument("--reuse", action="store_true", help="reuse slices already in --work")
    parser.add_argument("--check", action="store_true", help="do everything, write no fixture")
    parser.add_argument("--dry-run", action="store_true", help="say what would happen")
    parser.add_argument("--list", action="store_true", help="list the named variants")
    args = parser.parse_args()

    if args.list:
        for variant, entry in VARIANTS.items():
            marks = "" if variant in STORED else "   (not stored by default)"
            flags = f"  +{' '.join(entry.args)}" if entry.args else ""
            print(f"{variant:20} {json.dumps(entry.patch)}{flags}{marks}")
        return 0

    if not args.slicer:
        parser.error("name the slicer, e.g. \"flatpak run com.bambulab.BambuStudio\"")
    slicer = shlex.split(args.slicer)
    if not shutil.which(slicer[0]):
        raise SystemExit(f"{slicer[0]}: not found. Pass the slicer as the first argument.")
    for entry in VARIANTS.values():
        coerce(entry.patch)

    # `--outputdir` has to be inside $HOME: the flatpak has its own /tmp, so a
    # path there is written into the sandbox and silently lost — the run still
    # exits 0 while printing `the parent path ... is not there, create it!`.
    if "flatpak" in args.slicer and not str(args.work.resolve()).startswith(str(Path.home())):
        raise SystemExit(f"--work {args.work} is outside $HOME, which a flatpak cannot write to")

    plate = args.plate or HERE / DEFAULT_PLATE
    if not plate.exists():
        raise SystemExit(f"{plate}: no such plate")

    binary = args.corbel or next(
        (
            path
            for path in (REPO / "target/release/corbel", REPO / "target/debug/corbel")
            if path.exists()
        ),
        None,
    )
    if binary is None:
        raise SystemExit("nothing to verify with: cargo build --release, or pass --corbel")
    binary = Path(binary).resolve()

    if args.set:
        if not args.name:
            raise SystemExit("an ad-hoc --set needs a --name: the fixture is what it is called")
        patch: dict[str, object] = {}
        for entry in args.set:
            key, _, value = entry.partition("=")
            if not value:
                raise SystemExit(f"--set {entry}: wants KEY=VALUE")
            patch[key] = value
        VARIANTS[args.name] = v(coerce(patch))
        names = [args.name]
    elif args.variant:
        names = [v.strip() for entry in args.variant for v in entry.split(",") if v.strip()]
        if "all" in names:
            names = list(VARIANTS)
        unknown = [v for v in names if v not in VARIANTS]
        if unknown:
            raise SystemExit(f"unknown variant(s): {', '.join(unknown)} — --list says what there is")
    else:
        names = list(STORED)

    print(
        json.dumps(
            {
                "plate": str(plate),
                "plate_sha": sha(plate.read_bytes()),
                "window": f"layers {args.start} to {args.start + args.layers}",
                "slicer": args.slicer,
                "corbel": str(binary),
                "into": str(args.into),
                "work": str(args.work),
                "variants": names,
            },
            indent=2,
        )
    )
    if args.dry_run:
        return 0

    args.work.mkdir(parents=True, exist_ok=True)
    args.into.mkdir(parents=True, exist_ok=True)

    def run(slug: str, base: dict[str, object] | None) -> dict[str, object]:
        patch = VARIANTS.get(slug, VARIANTS["baseline"]).patch
        return measure(
            slug, patch, plate, args.work, slicer, binary, base, args.start, args.layers, args.reuse
        )

    # The baseline first, then the same untouched plate a second time. The
    # slicer does not reproduce itself, so the pair says how much of any
    # difference is noise: every variant has to clear that floor in one of
    # corbel's own counts before it counts as having changed anything.
    baseline = run("baseline", None)
    again = run("baseline-again", baseline)
    floor = {key: abs(again[key] - baseline[key]) for key in ("loops", "raised", "filament")}
    print(f"\nnoise floor between two baseline slices: {floor}")

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        drawn = list(pool.map(lambda variant: run(variant, baseline), names))
    rows = [baseline, *(row for row in drawn if row["name"] != "baseline")]

    print(f"\n{'variant':18} {'window':>9} {'loops':>6} {'raised':>7} {'filament':>10}  state")
    faults: list[str] = []
    for row in rows:
        target = args.into / fixture_name(row["name"])
        moved = [key for key in floor if abs(row[key] - baseline[key]) > floor[key]]
        evidence = [*row["echoed"]]
        if row["expects"] == "declined":
            evidence.append(
                "nothing raised, file untouched"
                if not row["changed"]
                else "nothing raised"
            )
        else:
            evidence += [
                f"{key} {row[key] - baseline[key]:+.0f}"
                for key in moved
                if key != "filament"
            ]
        # The file naming the new value says the setting took; only corbel's
        # own counts say the fixture reaches anything. A variant that changes
        # the settings block and nothing else is worth knowing about — on this
        # plate `enable_prime_tower` is one, because Bambu builds a tower only
        # for a plate that changes filament.
        if row["echoed"] and not moved:
            evidence.append("settings only")
        if row["expects"] == "declined":
            # This one is stored for the opposite reason: it is the case the
            # transform must decline rather than reach. Saying so is the check
            # — a run that raises a loop here has bricked a fill whose strands
            # do not stack, and the nozzle comes back through every ridge it
            # left. Compared against the baseline's own counts it would always
            # look "moved": this plate's fill is its whole body, so it has no
            # perimeter region to count at all.
            if row["raised"]:
                faults.append(
                    f"{row['name']}: this variant is the case the transform must "
                    f"decline, and it raised {row['raised']:.0f} loops"
                )
                state = "RAISED WHERE IT MUST NOT"
            elif args.check:
                state = "not written (--check)"
            else:
                state = write_fixture(
                    target,
                    (args.work / "windows" / f"{row['name']}.gcode").read_bytes(),
                    args.work,
                )
        elif row["stuck"]:
            faults.append(
                f"{row['name']}: did not take — "
                + "; ".join(f"{key} stayed at {value}" for key, value in row["stuck"].items())
            )
            state = "SETTING DID NOT TAKE"
        elif row["name"] != "baseline" and not row["echoed"] and not moved:
            faults.append(
                f"{row['name']}: nothing in the file says the setting took"
                + (f" ({', '.join(row['blind'])} is not echoed)" if row["blind"] else "")
                + f", and corbel counts the same loops, raises and filament as the baseline"
            )
            state = "SAME AS BASELINE"
        elif args.check:
            state = "not written (--check)"
        else:
            state = write_fixture(
                target,
                (args.work / "windows" / f"{row['name']}.gcode").read_bytes(),
                args.work,
            )
        print(
            f"{row['name']:18} {row['bytes'] / 1e6:8.2f}M {row['loops']:6.0f} {row['raised']:7.0f} "
            f"{row['filament']:10.1f}  {state}{('  · ' + ', '.join(evidence)) if evidence else ''}"
        )

    if faults:
        print("", file=sys.stderr)
        print("\n".join(f"  {fault}" for fault in faults), file=sys.stderr)
        return 1
    print(f"\n{args.into}: {len(rows)} fixtures, each checked against the baseline")
    print(f"slicer: {baseline['stamp']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
