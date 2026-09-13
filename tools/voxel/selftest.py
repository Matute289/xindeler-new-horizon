#!/usr/bin/env python3
"""Self-test for `voxlib`: round-trip, palette ranges, and the known traps.

    python3 tools/voxel/selftest.py

Pure Python, no dependencies, no engine build required. It does NOT prove the
engine renders the result — for that, run the throwaway `Segment` probe from
`references/05-authoring-workflow.md` in the `xindeler-voxel-authoring` skill.
"""

import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from voxlib import (  # noqa: E402
    GLOWY, SHINY, Palette, VoxModel, lint, read_vox, write_vox,
)

FAILURES = []


def check(name, cond, detail=""):
    if cond:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name} {detail}")
        FAILURES.append(name)


def main() -> int:
    tmp = tempfile.mkdtemp(prefix="voxlib-selftest-")
    print("palette")
    pal = Palette()
    matte = pal.add((10, 20, 30))
    shiny = pal.add((1, 2, 3), SHINY)
    glowy = pal.add((4, 5, 6), GLOWY)
    check("matte allocated outside the reserved band", matte >= 22, f"got {matte}")
    check("shiny allocated in 8..12", 8 <= shiny <= 12, f"got {shiny}")
    check("glowy allocated in 13..15", 13 <= glowy <= 15, f"got {glowy}")
    check("same colour+material is reused", pal.add((10, 20, 30)) == matte)
    check("different material gets a different slot", pal.add((10, 20, 30), GLOWY) != matte)
    pal.reserve([matte + 1])
    check("reserve is honoured", pal.add((99, 99, 99)) != matte + 1)
    try:
        for _ in range(10):
            pal.add((0, 0, len(pal.colors)), GLOWY)
        check("glowy exhaustion raises", False, "no exception")
    except ValueError:
        check("glowy exhaustion raises", True)

    print("model")
    m = VoxModel(name="probe")
    m.box((-2, -2, -2), (2, 2, 2), matte)
    check("box voxel count", len(m) == 125, f"got {len(m)}")
    m2 = VoxModel().sphere((0, 0, 0), 5, matte)
    solid = len(m2)
    m2.shell()
    check("shell removes interior", len(m2) < solid, f"{solid} -> {len(m2)}")
    m3 = VoxModel().box((1, 0, 0), (3, 0, 0), matte).mirror_x(plane=0)
    check("mirror_x merges both halves", len(m3) == 6, f"got {len(m3)}")
    check("mirror_x reflects correctly", m3.get(-3, 0, 0) == matte)
    shifted, size, origin = m.normalised()
    check("normalised size", size == (5, 5, 5), f"got {size}")
    check("normalised origin", origin == (-2, -2, -2), f"got {origin}")
    check("normalised starts at zero", shifted.get(0, 0, 0) == matte)

    print("limits")
    try:
        VoxModel().set(0, 0, 0, 255)
        check("index 255 rejected", False, "no exception")
    except ValueError:
        check("index 255 rejected", True)
    try:
        VoxModel().box((0, 0, 0), (300, 0, 0), matte).normalised()
        check("oversized model rejected", False, "no exception")
    except ValueError:
        check("oversized model rejected", True)

    print("round-trip")
    path = os.path.join(tmp, "rt.vox")
    a = VoxModel().box((0, 0, 0), (3, 2, 1), matte)
    b = VoxModel().sphere((0, 0, 0), 3, glowy)
    origins = write_vox(path, [a, b], pal)
    check("write returns one origin per model", len(origins) == 2, f"got {origins}")
    models, rpal = read_vox(path)
    check("model count survives", len(models) == 2, f"got {len(models)}")
    check("voxel counts survive", [len(x) for x in models] == [len(a), len(b)])
    check("palette survives", rpal.colors[matte] == (10, 20, 30, 255),
          f"got {rpal.colors[matte]}")
    check("indices survive (no off-by-one)", set(models[0].voxels.values()) == {matte})
    check("read reserves used indices", rpal.add((7, 7, 7), GLOWY) != glowy)
    check("header is VOX ", open(path, "rb").read(4) == b"VOX ")

    print("SIZE preservation (a shipped asset must not move on re-write)")
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    src = os.path.join(repo, "assets/voxygen/voxel/object/portal.vox")
    if os.path.exists(src) and open(src, "rb").read(4) == b"VOX ":
        smodels, spal = read_vox(src)
        declared = smodels[0].declared_size
        out = os.path.join(tmp, "portal.vox")
        origins2 = write_vox(out, smodels, spal)
        rmodels, _ = read_vox(out)
        check("declared SIZE survives a read->write round-trip",
              rmodels[0].declared_size == declared,
              f"{declared} -> {rmodels[0].declared_size}")
        check("re-write does not translate the asset", origins2 == [(0, 0, 0)],
              f"got {origins2}")
        check("voxel count survives", len(rmodels[0]) == len(smodels[0]))
        smodels[0].translate(0, 0, 5)
        _, _, moved_origin = smodels[0].normalised()
        check("an edited model re-bases to its real content",
              moved_origin != (0, 0, 0), f"got {moved_origin}")
    else:
        print("  skip (portal.vox unavailable — LFS blob not fetched?)")

    print("lint")
    good = VoxModel().box((0, 0, 0), (2, 2, 2), matte)
    check("clean model lints clean", lint([good], pal) == [])
    deliberate = VoxModel().box((0, 0, 0), (2, 2, 2), glowy)
    check("deliberate glow is not flagged", lint([deliberate], pal) == [])
    stray = Palette()
    stray.set(13, (1, 1, 1))
    accidental = VoxModel().box((0, 0, 0), (2, 2, 2), 13)
    check("accidental glow is flagged",
          any("GLOWY" in p for p in lint([accidental], stray)),
          lint([accidental], stray))
    unalloc = VoxModel().box((0, 0, 0), (2, 2, 2), 90)
    check("never-allocated index is flagged",
          any("never allocated" in p for p in lint([unalloc], Palette())),
          lint([unalloc], Palette()))
    hollow = VoxModel().box((0, 0, 0), (2, 2, 2), 16)
    check("index 16 is flagged", any("16" in p for p in lint([hollow], stray)))
    humanoid = VoxModel().box((0, 0, 0), (2, 2, 2), 0)
    check("humanoid clash is flagged",
          any("humanoid" in p for p in lint([humanoid], stray, humanoid=True)))
    check("empty model is flagged", lint([VoxModel()], pal) != [])
    big = VoxModel().box((0, 0, 0), (40, 40, 10), matte)
    check("figure kind accepts 41 voxels wide", not any(
        "mesher asserts" in p for p in lint([big], pal)))
    check("sprite kind rejects 41 voxels wide", any(
        "mesher asserts" in p for p in lint([big], pal, kind="sprite")),
        lint([big], pal, kind="sprite"))
    check("particle kind rejects it too", any(
        "mesher asserts" in p for p in lint([big], pal, kind="particle")))
    try:
        lint([good], pal, kind="nonsense")
        check("unknown kind rejected", False, "no exception")
    except ValueError:
        check("unknown kind rejected", True)

    print()
    if FAILURES:
        print(f"{len(FAILURES)} failure(s): {', '.join(FAILURES)}")
        return 1
    print("all voxlib self-tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
