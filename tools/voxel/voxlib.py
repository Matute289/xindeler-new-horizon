"""Hand-authoring of MagicaVoxel `.vox` assets for Xindeler, in plain Python.

This is the *code-authored* voxel path: you place voxels programmatically
(parametric shapes, symmetry, palette-exact recolours, batch variants,
voxel-by-voxel edits of an existing asset) instead of sculpting in an editor
or generating a mesh with an AI service.

Everything here is written against what the **engine actually reads**, which
is a deliberately small subset of the `.vox` format:

    MAIN
      SIZE  + XYZI   (one pair per model)
      RGBA           (256 palette entries)

`xindeler-common`'s `Segment::from_vox` (`common/src/figure/mod.rs`) reads
`models[model_index]` and `palette` and **nothing else** — no scene graph, no
layers, no materials. Every shipped figure asset in `assets/voxygen/voxel/`
has `scenes = 0, layers = 0, materials = 0`, so files written by this module
are structurally identical to the ones already in the repo.

Palette indices are load-bearing: see `INDEX_*` below and
`references/02-palette-and-materials.md` in the `xindeler-voxel-authoring`
skill.

No third-party dependencies. Python 3.9+.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from typing import Callable, Dict, Iterable, List, Optional, Sequence, Tuple

Coord = Tuple[int, int, int]
Rgba = Tuple[int, int, int, int]

# ---------------------------------------------------------------------------
# Palette-index semantics — verified against `Cell::from_index`
# (common/src/figure/cell.rs) by round-tripping one voxel per index 0..24
# through the real engine loader.
# ---------------------------------------------------------------------------

#: Plain, unlit surface. The default for everything.
MATTE = "matte"
#: Minor reflective/translucent treatment (`render_alpha = 0.1` in the shader).
SHINY = "shiny"
#: Self-illuminating (`emitted_light += 20 * colour`). Feeds the bloom chain.
GLOWY = "glowy"

#: Index 16 does not draw: it *carves* whatever an earlier-layered segment put
#: at that position (used by hats deleting hair, etc.). Only meaningful when
#: several segments are unioned with `DynaUnionizer::unify_with`.
INDEX_HOLLOW = 16
#: Indices 17..=21 draw as matte AND refuse to be carved by a later index-16.
INDICES_OVERRIDE_HOLLOW = range(17, 22)
#: Indices reserved by the humanoid `MatSegment` path (skin/hair/eyes recolour).
#: Harmless on non-humanoid bodies, catastrophic on `Body::Humanoid` art.
HUMANOID_MATERIAL_INDICES = {0: "Skin", 1: "Hair", 2: "EyeDark", 3: "EyeLight",
                             4: "SkinDark", 5: "SkinLight", 7: "EyeWhite"}

_SHINY_RANGE = range(8, 13)      # 8..=12
_GLOWY_RANGE = range(13, 16)     # 13..=15
#: Ordinary colours are allocated from here up. 0..21 are reserved (see above)
#: and 255 is unusable: `dot_vox` serialises index `i` as `i + 1`, so 255
#: overflows a u8 and panics `write_vox`. Verified by running it.
_MATTE_RANGE = range(22, 255)

MAX_INDEX = 254
#: Voxel coordinates are single bytes in XYZI, so no model may exceed this.
MAX_DIM = 256


# ---------------------------------------------------------------------------
# Palette
# ---------------------------------------------------------------------------


class Palette:
    """A 256-entry indexed palette that respects the engine's index ranges.

    Use :meth:`add` and let it pick a legal index for the material you want;
    only use :meth:`set` when you deliberately need a specific index (e.g.
    :data:`INDEX_HOLLOW`, or matching an existing asset's palette).
    """

    _RANGES = {MATTE: _MATTE_RANGE, SHINY: _SHINY_RANGE, GLOWY: _GLOWY_RANGE}

    def __init__(self, fill: Rgba = (0, 0, 0, 255)) -> None:
        self.colors: List[Rgba] = [fill] * 256
        self._by_key: Dict[Tuple[Rgba, str], int] = {}
        #: Indices that must not be handed out by `add` — either already
        #: allocated here, or in use by an asset this palette was read from.
        self._claimed: set = set()
        #: What `add` was *asked* for, per index. Lets `lint` tell a deliberate
        #: glow from an accidental one.
        self.intent: Dict[int, str] = {}

    def add(self, color: Sequence[int], material: str = MATTE) -> int:
        """Allocate (or reuse) an index holding `color` with `material`.

        Never returns an index that is already claimed — so you can read an
        existing asset's palette, add a colour to it, and be sure you are not
        silently repainting one of its existing materials.
        """
        rgba = _as_rgba(color)
        key = (rgba, material)
        if key in self._by_key:
            return self._by_key[key]
        if material not in self._RANGES:
            raise ValueError(f"unknown material {material!r}")
        for idx in self._RANGES[material]:
            if idx not in self._claimed:
                break
        else:
            raise ValueError(
                f"no free {material} slot left; {material} owns indices "
                f"{self._RANGES[material].start}..={self._RANGES[material].stop - 1}")
        self.colors[idx] = rgba
        self._by_key[key] = idx
        self._claimed.add(idx)
        self.intent[idx] = material
        return idx

    def set(self, index: int, color: Sequence[int], material: Optional[str] = None) -> int:
        """Write `color` at an exact index. Returns the index, for chaining."""
        if not 0 <= index <= MAX_INDEX:
            raise ValueError(f"palette index {index} out of range 0..={MAX_INDEX}")
        self.colors[index] = _as_rgba(color)
        self._claimed.add(index)
        if material is not None:
            self.intent[index] = material
        return index

    def reserve(self, indices: Iterable[int]) -> "Palette":
        """Mark indices as in-use so `add` will not reallocate them."""
        self._claimed.update(int(i) for i in indices)
        return self

    def material_of(self, index: int) -> str:
        if index in _SHINY_RANGE:
            return SHINY
        if index in _GLOWY_RANGE:
            return GLOWY
        return MATTE

    def to_bytes(self) -> bytes:
        return b"".join(struct.pack("<4B", *c) for c in self.colors)

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"<Palette {len(self._claimed)} claimed, {len(self._by_key)} allocated>"


def _as_rgba(color: Sequence[int]) -> Rgba:
    if len(color) == 3:
        r, g, b = color
        a = 255
    elif len(color) == 4:
        r, g, b, a = color
    else:
        raise ValueError(f"expected an (r, g, b) or (r, g, b, a) tuple, got {color!r}")
    for c in (r, g, b, a):
        if not 0 <= int(c) <= 255:
            raise ValueError(f"colour channel out of range in {color!r}")
    return (int(r), int(g), int(b), int(a))


# ---------------------------------------------------------------------------
# Model
# ---------------------------------------------------------------------------


@dataclass
class VoxModel:
    """A sparse voxel grid: `(x, y, z) -> palette index`.

    Axes match the engine's: **+X right, +Y forward, +Z up** (the same
    right-handed Z-up convention MagicaVoxel uses). Coordinates may be
    negative while you build; :meth:`normalised` shifts them into the
    0..255 range the file format allows.
    """

    voxels: Dict[Coord, int] = field(default_factory=dict)
    name: str = ""
    #: The extent declared by the source file's SIZE chunk, when this model
    #: came from :func:`read_vox`. Preserved so a read→write round-trip does
    #: not silently translate an asset whose voxels don't touch the min
    #: corner — several shipped assets don't (`portal.vox` starts at (1,1,0)).
    declared_size: Optional[Coord] = None

    # -- primitives --------------------------------------------------------

    def set(self, x: int, y: int, z: int, index: int) -> "VoxModel":
        if not 0 <= index <= MAX_INDEX:
            raise ValueError(f"palette index {index} out of range 0..={MAX_INDEX}")
        self.voxels[(int(x), int(y), int(z))] = int(index)
        return self

    def clear(self, x: int, y: int, z: int) -> "VoxModel":
        self.voxels.pop((int(x), int(y), int(z)), None)
        return self

    def get(self, x: int, y: int, z: int) -> Optional[int]:
        return self.voxels.get((int(x), int(y), int(z)))

    def box(self, lo: Coord, hi: Coord, index: int) -> "VoxModel":
        """Filled axis-aligned box, inclusive of both corners."""
        (x0, y0, z0), (x1, y1, z1) = _ordered(lo, hi)
        for x in range(x0, x1 + 1):
            for y in range(y0, y1 + 1):
                for z in range(z0, z1 + 1):
                    self.set(x, y, z, index)
        return self

    def ellipsoid(self, center: Coord, radii: Coord, index: int,
                  hollow: bool = False) -> "VoxModel":
        """Filled (or hollow) ellipsoid.

        ⚠️ `hollow=True` has the same cost inversion as :meth:`shell` — it adds
        an inward-facing surface rather than saving anything. Use it only when
        the inside is meant to be seen, e.g. a translucent shell over a glowing
        core.
        """
        cx, cy, cz = center
        rx, ry, rz = (max(float(r), 0.5) for r in radii)
        for x in range(int(cx - rx) - 1, int(cx + rx) + 2):
            for y in range(int(cy - ry) - 1, int(cy + ry) + 2):
                for z in range(int(cz - rz) - 1, int(cz + rz) + 2):
                    d = ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 + ((z - cz) / rz) ** 2
                    if d <= 1.0:
                        if hollow and d <= 0.55:
                            continue
                        self.set(x, y, z, index)
        return self

    def sphere(self, center: Coord, radius: float, index: int,
               hollow: bool = False) -> "VoxModel":
        return self.ellipsoid(center, (radius, radius, radius), index, hollow)

    def cylinder(self, base: Coord, radius: float, height: int, index: int,
                 axis: str = "z") -> "VoxModel":
        bx, by, bz = base
        for h in range(int(height)):
            for u in range(int(-radius) - 1, int(radius) + 2):
                for v in range(int(-radius) - 1, int(radius) + 2):
                    if u * u + v * v > radius * radius:
                        continue
                    if axis == "z":
                        self.set(bx + u, by + v, bz + h, index)
                    elif axis == "y":
                        self.set(bx + u, by + h, bz + v, index)
                    elif axis == "x":
                        self.set(bx + h, by + u, bz + v, index)
                    else:
                        raise ValueError(f"axis must be x/y/z, got {axis!r}")
        return self

    def capsule(self, a: Coord, b: Coord, radius: float, index: int) -> "VoxModel":
        """A swept sphere from `a` to `b` — the workhorse for limbs and tails."""
        ax, ay, az = a
        bx, by, bz = b
        dx, dy, dz = bx - ax, by - ay, bz - az
        length = max((dx * dx + dy * dy + dz * dz) ** 0.5, 1e-6)
        steps = int(length * 2) + 1
        for s in range(steps + 1):
            t = s / steps
            self.sphere((round(ax + dx * t), round(ay + dy * t), round(az + dz * t)),
                        radius, index)
        return self

    def cone(self, base: Coord, radius: float, height: int, index: int) -> "VoxModel":
        bx, by, bz = base
        for h in range(int(height)):
            r = radius * (1.0 - h / max(height, 1))
            for u in range(int(-radius) - 1, int(radius) + 2):
                for v in range(int(-radius) - 1, int(radius) + 2):
                    if u * u + v * v <= r * r:
                        self.set(bx + u, by + v, bz + h, index)
        return self

    def torus(self, center: Coord, major: float, minor: float, index: int) -> "VoxModel":
        """A ring in the XY plane — rune circles, halos, orbit bands."""
        cx, cy, cz = center
        lim = int(major + minor) + 1
        for x in range(-lim, lim + 1):
            for y in range(-lim, lim + 1):
                q = ((x * x + y * y) ** 0.5) - major
                for z in range(-int(minor) - 1, int(minor) + 2):
                    if q * q + z * z <= minor * minor:
                        self.set(cx + x, cy + y, cz + z, index)
        return self

    def line(self, a: Coord, b: Coord, index: int) -> "VoxModel":
        return self.capsule(a, b, 0.5, index)

    def shell(self) -> "VoxModel":
        """Delete every voxel that has all six neighbours filled.

        ⚠️ **This does not make the model cheaper to render — it makes it more
        expensive.** The greedy mesher emits a quad at every filled↔empty
        boundary with no visibility culling (`should_draw_greedy`,
        `voxygen/src/mesh/segment.rs:437`), so hollowing a solid volume adds a
        whole second, invisible, inward-facing surface and roughly doubles the
        quads and atlas usage. Interior voxels of a solid model are already
        free at render time.

        Use it only when the interior is genuinely meant to be seen (a dome, a
        broken shell, an open vessel), or to cut generator memory and file
        size for a very large prop where you accept the extra faces.
        """
        keep = {}
        for (x, y, z), i in self.voxels.items():
            neighbours = [(x + 1, y, z), (x - 1, y, z), (x, y + 1, z),
                          (x, y - 1, z), (x, y, z + 1), (x, y, z - 1)]
            if not all(n in self.voxels for n in neighbours):
                keep[(x, y, z)] = i
        self.voxels = keep
        return self

    # -- transforms --------------------------------------------------------

    def translate(self, dx: int, dy: int, dz: int) -> "VoxModel":
        self.voxels = {(x + dx, y + dy, z + dz): i for (x, y, z), i in self.voxels.items()}
        return self

    def mirror_x(self, plane: Optional[float] = None, merge: bool = True) -> "VoxModel":
        """Reflect across a YZ plane. With `merge`, keeps the original half too.

        Note the engine has its own mirroring (`Segment::from_vox(flipped=…)`,
        used by the `*_lateral_manifest.ron` specs to reuse one limb model for
        both sides) — prefer that when the asset is a left/right pair, and use
        this when you want one symmetric model.
        """
        if not self.voxels:
            return self
        if plane is None:
            xs = [x for x, _, _ in self.voxels]
            plane = (min(xs) + max(xs)) / 2.0
        mirrored = {(int(round(2 * plane - x)), y, z): i
                    for (x, y, z), i in self.voxels.items()}
        self.voxels = {**self.voxels, **mirrored} if merge else mirrored
        return self

    def recolor(self, mapping: Dict[int, int]) -> "VoxModel":
        """Palette-exact recolour: `{old_index: new_index}`."""
        self.voxels = {p: mapping.get(i, i) for p, i in self.voxels.items()}
        return self

    def map_indices(self, fn: Callable[[Coord, int], Optional[int]]) -> "VoxModel":
        """Per-voxel edit; return `None` from `fn` to delete the voxel."""
        out: Dict[Coord, int] = {}
        for pos, i in self.voxels.items():
            new = fn(pos, i)
            if new is not None:
                out[pos] = new
        self.voxels = out
        return self

    def union(self, other: "VoxModel", offset: Coord = (0, 0, 0)) -> "VoxModel":
        ox, oy, oz = offset
        for (x, y, z), i in other.voxels.items():
            self.set(x + ox, y + oy, z + oz, i)
        return self

    # -- bookkeeping -------------------------------------------------------

    def bounds(self) -> Tuple[Coord, Coord]:
        if not self.voxels:
            return (0, 0, 0), (0, 0, 0)
        xs = [p[0] for p in self.voxels]
        ys = [p[1] for p in self.voxels]
        zs = [p[2] for p in self.voxels]
        return (min(xs), min(ys), min(zs)), (max(xs), max(ys), max(zs))

    def normalised(self) -> Tuple["VoxModel", Coord, Coord]:
        """Shift so the minimum corner sits at the origin.

        Returns `(model, size, original_min)`. Keep `original_min`: it is what
        you add to the manifest `offset` so the part stays where you designed
        it relative to the bone pivot.

        If this model came from :func:`read_vox` and still fits inside the
        extent its source file declared, that extent is preserved and
        `original_min` is `(0, 0, 0)` — so re-writing a shipped asset does not
        move it relative to the manifest offset it was tuned against.
        """
        (mnx, mny, mnz), (mxx, mxy, mxz) = self.bounds()
        if self.declared_size is not None and self.voxels:
            dx, dy, dz = self.declared_size
            if (mnx >= 0 and mny >= 0 and mnz >= 0
                    and mxx < dx and mxy < dy and mxz < dz):
                return (VoxModel(dict(self.voxels), self.name, self.declared_size),
                        (dx, dy, dz), (0, 0, 0))
        shifted = VoxModel({(x - mnx, y - mny, z - mnz): i
                            for (x, y, z), i in self.voxels.items()}, self.name)
        size = (mxx - mnx + 1, mxy - mny + 1, mxz - mnz + 1) if self.voxels else (0, 0, 0)
        for axis, n in zip("xyz", size):
            if n > MAX_DIM:
                raise ValueError(
                    f"model {self.name or '<unnamed>'} is {n} voxels on {axis}; "
                    f"`.vox` coordinates are single bytes, max {MAX_DIM}")
        return shifted, size, (mnx, mny, mnz)

    def __len__(self) -> int:
        return len(self.voxels)


def _ordered(lo: Coord, hi: Coord) -> Tuple[Coord, Coord]:
    return ((min(lo[0], hi[0]), min(lo[1], hi[1]), min(lo[2], hi[2])),
            (max(lo[0], hi[0]), max(lo[1], hi[1]), max(lo[2], hi[2])))


# ---------------------------------------------------------------------------
# Writing
# ---------------------------------------------------------------------------


def _chunk(chunk_id: bytes, content: bytes, children: bytes = b"") -> bytes:
    assert len(chunk_id) == 4
    return chunk_id + struct.pack("<II", len(content), len(children)) + content + children


def write_vox(path: str, models: Iterable[VoxModel], palette: Palette,
              version: int = 150) -> List[Coord]:
    """Write `models` to `path`. Returns each model's original min-corner.

    Those corners are what you feed back into the RON manifest `offset`
    fields — the file itself stores no position, only a size and voxels
    starting at (0, 0, 0).
    """
    body = b""
    origins: List[Coord] = []
    for model in models:
        shifted, size, origin = model.normalised()
        origins.append(origin)
        body += _chunk(b"SIZE", struct.pack("<III", *size))
        xyzi = struct.pack("<I", len(shifted.voxels))
        for (x, y, z), i in sorted(shifted.voxels.items()):
            # `.vox` stores 1-based palette indices; `dot_vox` hands the engine
            # `i - 1`, so writing `i + 1` here means the engine sees exactly `i`.
            xyzi += struct.pack("<4B", x, y, z, i + 1)
        body += _chunk(b"XYZI", xyzi)
    body += _chunk(b"RGBA", palette.to_bytes())

    with open(path, "wb") as f:
        f.write(b"VOX " + struct.pack("<I", version))
        # `_chunk` already appends the children, so MAIN is written exactly once.
        f.write(_chunk(b"MAIN", b"", body))
    return origins


# ---------------------------------------------------------------------------
# Reading — for editing an existing asset voxel-by-voxel
# ---------------------------------------------------------------------------


def read_vox(path: str) -> Tuple[List[VoxModel], Palette]:
    """Parse the subset of `.vox` the engine cares about.

    Scene-graph (`nTRN`/`nGRP`/`nSHP`), `MATL` and `LAYR` chunks are skipped,
    exactly as the engine skips them. If you re-write a file you read with
    this, any such chunks a MagicaVoxel-authored source had are dropped —
    which changes nothing about how the engine renders it.
    """
    with open(path, "rb") as f:
        data = f.read()
    if data[:4] != b"VOX ":
        raise ValueError(f"{path} is not a .vox file")

    models: List[VoxModel] = []
    palette = Palette()
    pos = 8
    pending_size: Optional[Coord] = None

    def walk(buf: bytes, start: int, end: int) -> None:
        nonlocal pending_size
        p = start
        while p + 12 <= end:
            cid = buf[p:p + 4]
            n_content, n_children = struct.unpack("<II", buf[p + 4:p + 12])
            c0 = p + 12
            c1 = c0 + n_content
            if cid == b"MAIN":
                walk(buf, c1, c1 + n_children)
            elif cid == b"SIZE":
                pending_size = struct.unpack("<III", buf[c0:c0 + 12])  # type: ignore[assignment]
            elif cid == b"XYZI":
                count = struct.unpack("<I", buf[c0:c0 + 4])[0]
                m = VoxModel(declared_size=pending_size)
                pending_size = None
                for k in range(count):
                    x, y, z, i = struct.unpack("<4B", buf[c0 + 4 + k * 4:c0 + 8 + k * 4])
                    if i == 0:
                        raise ValueError(
                            f"{path}: XYZI holds palette index 0, which the format "
                            f"reserves as 'unused' — the file is malformed")
                    m.set(x, y, z, i - 1)
                models.append(m)
            elif cid == b"RGBA":
                palette.colors = [tuple(buf[c0 + k:c0 + k + 4])  # type: ignore[misc]
                                  for k in range(0, min(n_content, 1024), 4)]
                palette.colors += [(0, 0, 0, 255)] * (256 - len(palette.colors))
            p = c1 + n_children

    walk(data, pos, len(data))
    # Every index the asset actually uses is off-limits to `Palette.add`, so
    # extending a shipped asset's palette can't silently repaint it.
    for m in models:
        palette.reserve(m.voxels.values())
    return models, palette


# ---------------------------------------------------------------------------
# Checks — run these before committing an asset
# ---------------------------------------------------------------------------


#: Per-consumer size ceilings, each a hard `assert!` in the mesher that panics
#: the client on first draw. `voxygen/src/mesh/segment.rs`: figure/terrain 512³,
#: sprite 32×32×64, particle 16×16×64.
MESH_LIMITS = {
    "figure": (512, 512, 512),
    "terrain": (512, 512, 512),
    "sprite": (32, 32, 64),
    "particle": (16, 16, 64),
}


def lint(models: Sequence[VoxModel], palette: Palette,
         humanoid: bool = False, kind: str = "figure") -> List[str]:
    """Return a list of problems the engine would show as wrong art, not errors.

    Almost every `.vox` mistake in this engine is silent: a voxel whose index
    is past the end of the palette is *dropped* without a warning by
    `Segment::from_vox` (and rendered *black* by `MatSegment::from_vox`, the
    humanoid path), and a voxel in the 8..15 band quietly becomes shiny or
    emissive.

    `kind` selects which consumer's size ceiling to check — see
    :data:`MESH_LIMITS`. Those are hard `assert!`s that panic the client, not
    silent failures, and the sprite and particle ones are much tighter than
    the figure one.
    """
    if kind not in MESH_LIMITS:
        raise ValueError(f"kind must be one of {sorted(MESH_LIMITS)}, got {kind!r}")
    limit = MESH_LIMITS[kind]
    problems: List[str] = []
    for n, model in enumerate(models):
        if not model.voxels:
            problems.append(f"model[{n}] is empty")
            continue
        try:
            _, size, _ = model.normalised()
        except ValueError as e:
            problems.append(f"model[{n}]: {e}")
            continue
        for axis, got, cap in zip("xyz", size, limit):
            if got > cap:
                problems.append(
                    f"model[{n}] is {got} voxels on {axis}; the {kind} mesher asserts "
                    f"<= {cap} and panics the client above it")
        used = set(model.voxels.values())
        for i in sorted(used):
            if i > MAX_INDEX:
                problems.append(f"model[{n}] uses index {i}; max is {MAX_INDEX}")
                continue
            if palette.colors[i] == (0, 0, 0, 0):
                problems.append(f"model[{n}] index {i} has a fully transparent palette entry")
        # An index nobody allocated still has a colour — the default fill,
        # usually opaque black. That renders as black voxels, not as an error,
        # so it is the most common silent authoring mistake.
        unallocated = sorted(i for i in used if i <= MAX_INDEX and i not in palette._claimed)
        if unallocated:
            problems.append(
                f"model[{n}] uses indices {unallocated} that were never allocated in this "
                f"palette — they will render as the palette's fill colour")
        # Only flag a special index when nobody asked for that material — a
        # colour allocated with `Palette.add(..., GLOWY)` is deliberate.
        accidental_glow = sorted(i for i in used & set(_GLOWY_RANGE)
                                 if palette.intent.get(i) != GLOWY)
        accidental_shiny = sorted(i for i in used & set(_SHINY_RANGE)
                                  if palette.intent.get(i) != SHINY)
        if accidental_glow:
            problems.append(
                f"model[{n}] uses indices {accidental_glow} without asking for GLOWY — "
                f"these will self-illuminate and bloom")
        if accidental_shiny:
            problems.append(
                f"model[{n}] uses indices {accidental_shiny} without asking for SHINY — "
                f"these will render at alpha 0.1")
        if INDEX_HOLLOW in used:
            problems.append(
                f"model[{n}] uses index {INDEX_HOLLOW}, which carves other segments away "
                f"instead of drawing")
        if humanoid:
            clashes = sorted(used & set(HUMANOID_MATERIAL_INDICES))
            if clashes:
                names = ", ".join(f"{i}={HUMANOID_MATERIAL_INDICES[i]}" for i in clashes)
                problems.append(
                    f"model[{n}] uses humanoid material indices ({names}); on a "
                    f"Body::Humanoid asset these are recoloured at runtime")
        if size[0] * size[1] * size[2] and len(model) > 60_000:
            problems.append(
                f"model[{n}] has {len(model)} voxels — large for a figure, which is "
                f"re-meshed per body variant into one shared texture atlas. Prefer a "
                f"smaller model; do NOT reach for `.shell()`, which adds an invisible "
                f"inner surface (see its docstring)")
    return problems


__all__ = [
    "Palette", "VoxModel", "write_vox", "read_vox", "lint",
    "MATTE", "SHINY", "GLOWY", "INDEX_HOLLOW", "INDICES_OVERRIDE_HOLLOW",
    "HUMANOID_MATERIAL_INDICES", "MAX_INDEX", "MAX_DIM",
]
