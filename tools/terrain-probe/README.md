# terrain-probe

Developer tooling, **never shipped**. It generates the real Cromatolis world
exactly as the server does (`World::generate` with the `world.map.cromatolis_v0`
map asset and the seed you pass), samples every 1 m column of a box, and writes a
`tprobe v1` dump that verifiers (and you) can read without running the engine.

It is a workspace member only so it can use the engine crates; no shipped crate
depends on it and no release, Docker or deploy pipeline builds it (they build
`xindeler-server-cli` / `xindeler-voxygen` by package name). It does not touch
the `default-publish` feature set.

```bash
# Build
cargo build -p xindeler-terrain-probe

# Always point it at the asset root you mean. The directory MUST be named `assets`
# and contain world/map/cromatolis_v0.bin (anything else silently falls back to the
# repo assets in the engine, so the tool refuses it).
export VELOREN_ASSETS=$PWD/assets          # or pass --assets DIR
```

## Subcommands

| Command | What |
|---|---|
| `dump --box x0,y0,x1,y1 --out F` | Fast path. `generate_chunk` for every chunk of the box (rayon), every 1 m column sampled, writes a `tprobe v1` file. `--zmin/--zmax` set the z range (default: automatic from the sim table, `lowest basement - 16` to `highest alt/water + 96`); `--margin` is the site report margin (default 600 m); `--seed` (default 0); `--keep-sprites` (see Reproducibility). |
| `cols --in F --line x0,y0,x1,y1 [--step 1] [--runs]` | Print the columns along a line (transect): alt, riverless alt, water level, ground top, top block kind, water top, liquid depth, flags, number of runs (or the full run list with `--runs`). |
| `sim (--in F \| --box ...)` | Per-chunk sim table: alt, water_alt, basement, chaos, river kind, cross-section, velocity, rockiness, cliff height, path, humidity, temp, tree density, flux, underwater. From a dump, or live (about 3 s of world generation, no chunk generation) with `--box` and `--pad` chunks of context. |
| `sites-near (--center x,y \| --box ...) --radius R` | Authored sites in the generated world **and** every authored point (settlements, landmarks, caves, bridges, interiors...) from `cromatolis_v0_*.ron`, with distance. `--require-empty` exits 1 if anything is found (use it to guarantee a test arena is empty); `--no-world` skips world generation and lists only the `.ron` points; `--json`. |
| `info --in F` | Header, counters, column statistics, section sizes. |
| `diff A B [--show N]` | Block-by-block comparison of two dumps of the same box and z range (class pairs, bounding box, examples, float and surface differences). Exit 1 on any difference. |
| `check-legacy --in F --legacy DIR` | Compare a dump with the dense `blocks.bin`/`cols.f32`/`meta.txt` of the 2026-10-03 research harness (same box and z range required). Exit 1 on any difference. |

Exit codes: 0 ok, 1 negative result (`--require-empty` hit, `check-legacy` differs), 2 error.

Examples:

```bash
# 1.2 x 1.06 km research arena, z 100..280
terrain-probe --assets $A dump --box 22750,24550,23950,25610 --zmin 100 --zmax 280 --out arena1.tprobe

# The same box must be empty of sites/landmarks before using it as an arena
terrain-probe --assets $A sites-near --box 22750,24550,23950,25610 --radius 600 --require-empty

# A 200 m transect along x at y=24900, runs included
terrain-probe cols --in arena1.tprobe --line 22800,24900,23000,24900 --step 1 --runs

# Sim chunks of an area without dumping blocks
terrain-probe --assets $A sim --box 22750,24550,23950,25610 --pad 2 --out sim.csv
```

## `tprobe v1` format

```
"TPROBE1\n"  u32-LE header_len  header JSON  14 zstd sections (in header.sections order)
```

Header JSON: `format`, `box_xy` `[x0,y0,x1,y1]` (metres, half-open), `zmin`, `zmax`
(half-open), `nx`, `ny`, `seed`, `path` (`fast`; `client` is produced by the client
dump), `calendar` (null), `engine_commit`, `assets` (sha256 of every
`cromatolis_v0*` file), `class_codes`, `block_kinds` (BlockKind name to code),
`stats`, `sections` (`name`, `raw_len`, `comp_len`).

Columns are row-major: `idx = (y - y0) * nx + (x - x0)` (row 0 = southernmost,
`numpy.reshape(ny, nx)`). Multi-byte arrays are little-endian and byte-shuffled
(all byte 0s, then byte 1s, ...) before zstd.

| Section | Type | Meaning |
|---|---|---|
| `alt`, `riverless_alt`, `water_level`, `warp_factor` | f32 x columns | the column sampler's float fields (NaN when the sampler returned none) |
| `top_kind` | u8 | `BlockKind` code of the topmost natural ground block (255 = none) |
| `flags` | u8 | 1 structure, 2 sprite, 4 void (non-solid run below the ground top: cave/shaft/authored void), 8 liquid, 16 clipped top (raise `--zmax`) |
| `ground_top` | i16 | z of the topmost natural ground block (-32768 = none) |
| `water_top` | i16 | z of the topmost liquid block; the water surface is `water_top + 1` |
| `liquid_depth` | u16 | liquid blocks above `ground_top` |
| `run_counts` | u16 x columns | runs per column |
| `run_class` / `run_len` | u8 / u16 | concatenated runs of every column, bottom (`zmin`) to top (`zmax`), run lengths sum to `zmax - zmin` |
| `sim_csv` | text | the `sim` table for the box plus 2 chunks of margin (`;`-separated, header row) |
| `sites_json` | JSON | sites and authored points within `--margin` of the box |

Block classes: `0` air, `1` natural ground (Rock, WeakRock, GlowingRock, Grass,
Snow, Earth, Sand, Ice), `2` liquid (water and lava), `3` unloaded (client path
only), `4` other solid, non-sprite block (structures, wood, leaves), `5`
sprite (only with `--keep-sprites`; otherwise sprites are recorded as air). Voids and air gaps are simply the air/liquid runs below the
ground top.

The file has no timestamp or host data and the writer uses a fixed zstd level; see
Reproducibility for what the engine itself makes non-deterministic.

## Reading a dump from Python

```python
import json, struct, numpy as np, zstandard   # pip install zstandard numpy

f = open("arena1.tprobe", "rb").read()
assert f[:8] == b"TPROBE1\n"
hl = struct.unpack("<I", f[8:12])[0]
h = json.loads(f[12:12 + hl]); o = 12 + hl
d = zstandard.ZstdDecompressor(); secs = {}
for s in h["sections"]:
    secs[s["name"]] = d.decompress(f[o:o + s["comp_len"]], max_output_size=s["raw_len"])
    o += s["comp_len"]

def arr(name, dt):                      # un-shuffle byte lanes
    w = np.dtype(dt).itemsize
    return np.frombuffer(secs[name], np.uint8).reshape(w, -1).T.copy().view(dt).ravel()

alt = arr("alt", "<f4").reshape(h["ny"], h["nx"])          # row 0 = southernmost
ground_top = arr("ground_top", "<i2").reshape(h["ny"], h["nx"])
```

(The Rust reader in `src/format.rs`, `Dump::read_file`, is the reference.)

## Reproducibility

The terrain, water, structure blocks and every per-column float are fully
deterministic: two dumps of the same box agree bit for bit. Two things in the
engine are **not**, because they come from the engine's per-chunk dynamic RNG
(`dynamic_rng` in `World::generate_chunk`):

- **Sprites** (flowers, tufts, boulders, chests): which cells hold one, and whether
  it is solid, changes per run. By default every sprite block is recorded as air so
  the dump stays byte-reproducible; `--keep-sprites` records class 5 instead (and the
  file is then not reproducible).
- **Cave decoration** (cavern mushrooms, crystals and similar, deep underground): a
  handful of rock/other-solid cells flip per run. On a 5 x 5 km box over the whole
  height range this was about 10 thousand cells out of 3.9e10, all deep underground,
  with identical floats, ground tops, water tops and top kinds.

So: boxes that stay above the caverns (the research arena, any surface box with an
explicit `--zmin`) give byte-identical files; boxes that include caverns give
files that agree on everything except those decoration cells. `terrain-probe diff A B`
quantifies exactly that (counts per class pair, bounding box, examples) and exits 1 on
any difference, and it reports surface columns (ground/water top, depth, top kind) separately.

## Scope and caveats

- The fast path equals what the client receives for terrain and water (measured
  bit-identical over 3.09 M columns), but it passes no regional terrain overrides
  and no calendar, so sprites can differ from a live server. Always cover the
  whole declared box: sparse transects are what hid the original water bugs.
- Speed (18-core Mac, dev profile): 1326 chunks (1.2 x 1.06 km) in 1.2 s; 24 649 chunks
  (5 x 5 km, 25 M columns) in about 37 s, roughly 40 000 chunks/min, plus ~3 s of world
  generation. Peak RAM for that 5 x 5 km box (z range -442..1126) was about 3.4 GB; a
  box with a tight `--zmin/--zmax` needs far less. The z range is the main cost and
  size driver, so pass one when you only care about the surface.
- `sim`'s river velocity is read from the column sampler at the chunk centre, and the
  river cross-section from the chunk's river kind, because the engine does not expose
  those fields directly.
- Tests: `cargo test -p xindeler-terrain-probe` runs the format round-trip tests.
  The real-asset test is `#[ignore]`d: `VELOREN_ASSETS=... cargo test -p
  xindeler-terrain-probe -- --ignored`.
