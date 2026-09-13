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

import struct  # noqa: E402

from voxlib import (  # noqa: E402
    GLOWY, SHINY, Palette, VoxModel, animation_frames, lint, read_scene,
    read_vox, write_vox,
)
from voxlib import _chunk  # noqa: E402

FAILURES = []


def _vox_string(s):
    raw = s.encode("utf-8")
    return struct.pack("<i", len(raw)) + raw


def _vox_dict(d):
    out = struct.pack("<i", len(d))
    for k, v in d.items():
        out += _vox_string(k) + _vox_string(v)
    return out


def write_scene_vox(path, parts, palette):
    """Test-only: write a `.vox` WITH a MagicaVoxel-style scene graph.

    `voxlib` deliberately never writes one (the engine ignores it), so the
    scene reader needs a fixture built here. `parts` is a list of
    `(VoxModel, name, translation, frame_index_or_None)`.
    """
    body = b""
    for model, _name, _t, _f in parts:
        shifted, size, _origin = model.normalised()
        body += _chunk(b"SIZE", struct.pack("<III", *size))
        xyzi = struct.pack("<I", len(shifted.voxels))
        for (x, y, z), i in sorted(shifted.voxels.items()):
            xyzi += struct.pack("<4B", x, y, z, i + 1)
        body += _chunk(b"XYZI", xyzi)
    body += _chunk(b"RGBA", palette.to_bytes())

    # Root nTRN(0) -> nGRP(1) -> per-part nTRN(2+2i) -> nSHP(3+2i)
    kids = [2 + 2 * i for i in range(len(parts))]
    body += _chunk(b"nTRN", struct.pack("<i", 0) + _vox_dict({})
                   + struct.pack("<iiii", 1, -1, 0, 1) + _vox_dict({}))
    body += _chunk(b"nGRP", struct.pack("<i", 1) + _vox_dict({})
                   + struct.pack("<i", len(kids))
                   + b"".join(struct.pack("<i", k) for k in kids))
    for i, (_model, name, t, f) in enumerate(parts):
        node = 2 + 2 * i
        frame = {"_t": f"{t[0]} {t[1]} {t[2]}"}
        if f is not None:
            frame["_f"] = str(f)
        body += _chunk(b"nTRN", struct.pack("<i", node) + _vox_dict({"_name": name})
                       + struct.pack("<iiii", node + 1, -1, 0, 1) + _vox_dict(frame))
        model_attrs = {"_f": str(f)} if f is not None else {}
        body += _chunk(b"nSHP", struct.pack("<i", node + 1) + _vox_dict({})
                       + struct.pack("<i", 1) + struct.pack("<i", i)
                       + _vox_dict(model_attrs))

    with open(path, "wb") as fh:
        fh.write(b"VOX " + struct.pack("<I", 150))
        fh.write(_chunk(b"MAIN", b"", body))


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

    print("scene graph (MagicaVoxel interop — the part the engine ignores)")
    check("a voxlib-written file has no scene graph", read_scene(path) == [])
    spath = os.path.join(tmp, "scene.vox")
    head = VoxModel().box((0, 0, 0), (2, 2, 2), matte)
    foot = VoxModel().box((0, 0, 0), (1, 1, 1), matte)
    write_scene_vox(spath, [(head, "head", (0, 0, 10), None),
                            (foot, "foot", (-3, 0, 0), 1)], pal)
    smodels2, _ = read_vox(spath)
    check("models still read normally alongside a scene graph",
          len(smodels2) == 2, f"got {len(smodels2)}")
    places = read_scene(spath)
    check("one placement per shape node", len(places) == 2, f"got {len(places)}")
    check("names are recovered", [p.name for p in places] == ["head", "foot"],
          f"got {[p.name for p in places]}")
    check("model ids are recovered", [p.model_id for p in places] == [0, 1])
    check("translations accumulate from the root",
          [p.translation for p in places] == [(0, 0, 10), (-3, 0, 0)],
          f"got {[p.translation for p in places]}")
    check("centre -> min corner conversion",
          places[0].min_corner((3, 3, 3)) == (-1, -1, 9),
          f"got {places[0].min_corner((3, 3, 3))}")
    check("frame index is recovered",
          [p.frame_index for p in places] == [None, 1],
          f"got {[p.frame_index for p in places]}")
    check("frames group by _f", len(animation_frames(places)) == 2,
          f"got {len(animation_frames(places))}")
    check("a static scene groups to a single frame",
          len(animation_frames([p for p in places if p.frame_index is None])) == 1)

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
