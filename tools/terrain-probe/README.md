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
| `dump --box x0,y0,x1,y1 --out F` | Fast path. `generate_chunk` for every chunk of the box (rayon), every 1 m column sampled, writes a `tprobe v1` file. `--zmin/--zmax` set the z range (default: automatic, `lowest basement/alt - 16` to `highest alt/water + 96`, taken over the chunks the box touches plus a one-chunk halo (a column's altitude is a spline of the neighbouring chunk knots) and widened to the column sampler's actual `alt`/`water_level` on a 4 m grid over the box; BUG-P8 clipped 938 of 1024 columns next to a 500 m step before this); `--margin` is the site report margin (default 600 m); `--seed` (default 0); `--keep-sprites` (see Reproducibility); `--force` skips the memory pre-flight. z must lie in `-32767..=32768` (z is stored as i16, -32768 means "none") and be at most 32767 blocks tall, otherwise the command errors. Before sampling, the estimated peak memory (about 100 bytes per column, 160 for `client-dump`) is checked against 8 GB and bigger boxes are refused unless `--force`. |
| `cols --in F --line x0,y0,x1,y1 [--step 1] [--runs]` | Print the columns along a line (transect): alt, riverless alt, water level, ground top, top block kind, water top, liquid depth, flags, number of runs (or the full run list with `--runs`). |
| `sim (--in F \| --box ...)` | Per-chunk sim table: alt, water_alt, basement, chaos, river kind, cross-section, velocity, rockiness, cliff height, path, humidity, temp, tree density, flux, underwater. From a dump, or live (about 3 s of world generation, no chunk generation) with `--box` and `--pad` chunks of context. |
| `sites-near (--center x,y \| --box ...) --radius R` | Authored sites in the generated world **and** every authored point (settlements, landmarks, caves, bridges, interiors...) from `cromatolis_v0_*.ron`, with distance. Authored linear features (routes, maritime routes, bridges, fortification walls) are tested by their **segments** against the box grown by the radius (a route whose vertices are kilometres away but whose span passes within the radius is reported, one record per feature at its point nearest the box centre); fortifications are converted from their source pixels (`px / (dim - 1)`, like the engine loader), the aerial citadel by its centre and `max_radius_m`, world sites by origin within radius + their own radius. Point and segment tests are both **closed** (a feature exactly `radius` away counts) and every authored feature gets 64 m of extra reach, because the engine snaps authored points to chunk centres on a `(chunks - 1)` grid while the tool maps linearly (test `ec_f01` measures the worst gap against the engine's own `cromatolis_source_pixel_to_wpos`: 25 m on the committed files). A `.ron` file declaring a coordinate space the tool cannot convert, or a file with no coordinate space that is not in the code's `NO_POSITION_FILES` allowlist, is an error (exit 2), never a silent skip (`dump` warns on stderr instead). `--require-empty` exits 1 if anything is found (use it to guarantee a test arena is empty); `--no-world` skips world generation and lists only the `.ron` points; `--json`. |
| `info --in F` | Header, counters, column statistics, section sizes. |
| `client-dump --box x0,y0,x1,y1 --out F` | **Client path.** Starts a throw-away `xindeler-server-cli`, teleports a headless admin bot along a tiling of the box, waits until every chunk is streamed and dumps the blocks the client holds (see "Client path" below). `--compare-fast` also runs the fast path and prints the discrepancy. |
| `diff A B [--show N] [--client-compare] [--landing-radius R]` | Block-by-block comparison of two dumps of the same box and z range (class pairs, bounding box, examples, float and surface differences). Exit 1 on any difference. `--client-compare` is the fast-vs-client mode: floats ignored, terrain/water differences tolerated within R m (default 8) of a recorded bot landing, structure/sprite-only differences reported but never failing. |
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

## Client path (`client-dump`)

```bash
cargo build -p xindeler-terrain-probe -p xindeler-server-cli
terrain-probe --assets $A client-dump --box 22750,24550,23950,25610 \
    --zmin 100 --zmax 280 --out arena_client.tprobe --compare-fast
```

What it does, and the exact settings it uses:

- **Server:** `xindeler-server-cli` (next to this executable, `--server-bin`, or
  `$TPROBE_SERVER_BIN`; it is *not* a build dependency) with `--no-auth
  --non-interactive`, `VELOREN_ASSETS` = the asset root of the probe (`--assets`),
  and `VELOREN_USERDATA` = a scratch dir `tprobe-client-dump-<pid>-<nanos>` under
  the OS temp dir (set `$TMPDIR` to put it elsewhere). It writes
  `server/server_config/settings.ron` with: one `Tcp` protocol on `127.0.0.1` and a
  **free port**, `world_seed` = `--seed` (default 0), `map_file:
  Some(LoadAsset("world.map.cromatolis_v0"))`, `auth_server_address/auth_service_address/
  query_address: None`, `calendar_mode: None` (the fast path has no calendar either)
  and `max_view_distance: Some(view_distance + 8)`; and `server-cli/settings.ron` with
  the web/metrics endpoint on a second free loopback port and no signal handlers. The
  user's real userdata, saves and ports (14004-14006) are never read or bound.
- **Bot:** user `tprobebot`, registered with `admin add tprobebot admin`, connects
  through the `xindeler-client` crate, creates a Human Warrior and teleports with
  `/goto x y z` (z = base altitude + 3). The server runs a real game tick, so the
  bot stands on the ground at each tile centre; those positions are stored in the
  dump header (`client.tiles[].landing`).
- **Tiling:** the client forgets chunks far from its player, so the box is split
  into tiles that each fit inside the view distance (`--view-distance`, default 24
  chunks gives tiles of up to 29x29 chunks; a 1.4 km box is 2x2 tiles). The bot
  visits one tile at a time, waits until all of the tile's chunks are present
  (`--timeout` per tile, default 600 s; after `--stall-secs` without a new chunk it
  is nudged and chunks are requested again), harvests them, then moves on.
- **Result:** the same `tprobe v1` file with `path = "client"`. Column floats
  (`alt`, `riverless_alt`, ...) are NaN because the client never receives them
  (`stats.floats_present = 0`); the sim table and sites come from the in-process
  world. The header gains a `client` object: view distance, chunks total/streamed,
  missing chunk keys (their blocks are class 3, unloaded) and per-tile goto,
  landing position, counts and seconds. The exit status is 1 when fewer than
  `--min-streamed` percent (default 99.9) of the chunks arrived or, with
  `--compare-fast`, when terrain/water differ outside the landing radius.
- **Cleanup:** only the server process it spawned is killed (by handle, never by
  name), also on Ctrl-C/SIGTERM and on any error; the scratch dir is deleted.
  `--keep-server-log` keeps the log as `<out>.server.log`.
- Dump timing: the server needs about 5 s to start on the research arena; a
  1.2 x 1.06 km box (1326 chunks) streams in about a minute on an 18-core Mac.

`scripts/arena_client_check.sh ASSETS_DIR [OUT_DIR]` runs the fast dump, the client
dump and the diff on the synthetic research arena box and prints the discrepancy
summary (it only reproduces spec 4.5 on the arena build of the map).

Tests: unit tests cover the tiling math, the scratch server's settings/ready-line/
cleanup guard and the discrepancy classification; the real end-to-end test is
`#[ignore]`d: `VELOREN_ASSETS=... cargo test -p xindeler-terrain-probe --
--ignored client_dump`.

## `tprobe v1` format

```
"TPROBE1\n"  u32-LE header_len  header JSON  14 zstd sections (in header.sections order)
```

Header JSON: `format`, `box_xy` `[x0,y0,x1,y1]` (metres, half-open), `zmin`, `zmax`
(half-open), `nx`, `ny`, `seed`, `path` (`fast`, or `client` from `client-dump`), `calendar` (null),
`engine_commit`, `assets` (sha256 of every
`cromatolis_v0*` file), `class_codes`, `block_kinds` (BlockKind name to code),
`stats`, `client` (client dumps only: view distance, streamed counts, missing chunks, tiles with
goto and landing positions), `sections` (`name`, `raw_len`, `comp_len`).

Columns are row-major: `idx = (y - y0) * nx + (x - x0)` (row 0 = southernmost,
`numpy.reshape(ny, nx)`). Multi-byte arrays are little-endian and byte-shuffled
(all byte 0s, then byte 1s, ...) before zstd.

| Section | Type | Meaning |
|---|---|---|
| `alt`, `riverless_alt`, `water_level`, `warp_factor` | f32 x columns | the column sampler's float fields (NaN when the sampler returned none) |
| `top_kind` | u8 | `BlockKind` code of the topmost natural ground block, i.e. the top class-1 block (255 = none) |
| `flags` | u8 | 1 structure, 2 sprite, 4 void (non-solid run below the ground top: cave/shaft/authored void), 8 liquid, 16 clipped top (raise `--zmax`), 32 structure above ground (a class-4 block above `ground_top`: wall, keep, tree, bridge deck, boulder; or structure blocks in a column with no ground). 64 lava (the column holds at least one lava block; class 2 is water *and* lava, so `liquid` with `lava` = lava present, `liquid` alone = water only; set by the probe from block kinds), 128 rim / no sample (the column sampler returned nothing, NaN `alt`: within roughly 32 m of the map's W/S border and 64 m of its E/N border, measured by `ec_d41_world_edge_boxes`, which asserts the flag matches the NaN columns and that the band is at most 64 m; such columns have no natural ground and read as water to the bottom, so verifiers must not treat them as terrain; what to do about the band is a map-level policy decision, T23). Bits 32, 64 and 128 were added within v1 (flags are bits only, the byte layout is unchanged; the header's `format_rev` / `flags_defined` say which bits a dump may use, see "Format revisions"); `cols` prints 32/64/128 as `A`/`L`/`R`. |
| `ground_top` | i16 | z of the topmost natural ground block (class 1; -32768 = none). Natural-kind blocks above the sampler's surface are class 4 and do not count (see Natural terrain vs structures) |
| `water_top` | i16 | z of the topmost liquid block; the water surface is `water_top + 1` |
| `liquid_depth` | u16 | liquid blocks above `ground_top` |
| `run_counts` | u16 x columns | runs per column |
| `run_class` / `run_len` | u8 / u16 | concatenated runs of every column, bottom (`zmin`) to top (`zmax`), run lengths sum to `zmax - zmin` |
| `sim_csv` | text | the `sim` table for the box plus 2 chunks of margin (`;`-separated, header row) |
| `sites_json` | JSON | sites and authored points within `--margin` of the box |

Block classes: `0` air, `1` natural terrain (a Rock, WeakRock, GlowingRock, GlowingWeakRock, Grass,
Snow, Earth, Sand or Ice block that is not above the column sampler's surface), `2` liquid (water and lava; lava columns carry flag 64), `3` unloaded (client path
only), `4` any other solid, non-sprite block (structures, wood, leaves, and natural-kind blocks standing above the sampler's surface), `5`
sprite (only with `--keep-sprites`; otherwise sprites are recorded as air). Voids and air gaps are simply the air/liquid runs below the
ground top.

The file has no timestamp or host data and the writer uses a fixed zstd level; see
Reproducibility for what the engine itself makes non-deterministic.

Reading is hardened against corrupt files: the header is capped at 16 MB, the
header must be self-consistent (box vs `nx`/`ny`, at most 2^31 columns, z range
as above), every fixed-size section must declare exactly the length the header
implies, no section may claim more than zstd's maximum expansion (40 000x) of its
compressed size, and buffers grow only as bytes actually arrive. A bad file
returns an error; it cannot allocate the size it declares. The writer compresses
2 sections at a time (each section is one zstd frame, so the bytes do not depend
on that) and drops every raw buffer right after compressing it.

### Natural terrain vs structures

A `BlockKind` alone cannot tell terrain from a structure: `Block` carries only a
kind, a colour and sprite data, and site/structure code places plain Rock, Earth
and Sand. So the classifier uses the column sampler, which is exactly what the
engine fills terrain from (`world/src/block.rs`: solid iff `z <= alt as i32`;
the one natural exception is Ice, frozen water at the water level, which sits
above `alt`). A block of a natural kind (Rock, WeakRock, GlowingRock, GlowingWeakRock, Grass,
Snow, Earth, Sand) with `z > trunc(alt)` is therefore **not terrain** and is
recorded as class 4. Consequences:

- `ground_top` / `top_kind` / `liquid_depth` / the `void` flag describe the
  terrain surface; a stone keep, wall, well, bridge deck or roof no longer raises
  `ground_top`, changes `top_kind`, or turns a building interior into a "void".
- Flag 32 (`structure above ground`) marks columns where such blocks, trees or
  other class-4 blocks stand on the surface. It includes trees and also
  procedural boulders/debris and authored floating features (aerial citadel
  pieces), all of which are "not sampler terrain". Use the runs for the exact
  geometry.
- Natural-kind blocks *below* `trunc(alt)` that a structure placed (foundations,
  cellars' walls) stay class 1; they cannot be told apart and do not affect the
  surface fields.
- If the sampler returned nothing for a column (NaN `alt`) the kind alone
  decides, as before. `client-dump` evaluates the in-process sampler for the
  surface only (its float columns stay NaN), so fast and client dumps classify
  identically.
- Measured on the research arena map: around the authored settlement Falsepost
  (400 x 400 m) 4 425 of 160 000 columns reported a `ground_top` up to 22 m above
  the terrain and 2 598 were flagged void; now 0 and 4. On a 5 x 5 km box
  245 213 of 25 M columns (aerial citadel pieces and boulders, up to 189 m above
  the terrain) no longer raise `ground_top`.

## Format revisions and the v2 plan

The magic (`TPROBE1`) and the 14-section layout never change within v1; two header keys say how to read the flags byte:

| `format_rev` | written by | `flags_defined` | notes |
|---|---|---|---|
| 0 (key absent) | every dump before the field existed | 1..=32 (`0x3F`) | bits 1-16 are the first v1 (commit 59de1ec248); bit 32 landed in the review round before the field. A rev-0 file serialises byte-identically to the old writer (the golden-hash test pins it). |
| 1 | this build | 1..=128 (`0xFF`, written explicitly) | adds `LAVA` (64) and `RIM_NO_SAMPLE` (128) |

A newer reader reads every older dump. This reader **refuses** a dump with a higher `format_rev`, with `flags_defined` bits it does not know, or whose flags use bits outside its own `flags_defined` (a rev-0 file that carries bit 64 is mislabelled). The 8-bit flags lane is now **full**: the next flag bit needs revision 2.

**v2 plan (task T21):** bump `format_rev` to 2 and either widen the flags lane to u16 (a new lane-shuffled section, readers of rev 2 take flags from it) or keep the u8 and add a `flags2` section; the unchanged sections stay byte-identical. The v2 list, from the verifier needs of the corner-case catalogue (F05, F16, F17):

- a per-block **liquid kind** (water / lava / future) instead of the per-column `LAVA` flag, e.g. a parallel run class or a `liquid_kind` section;
- **solid sprites** (boulders, chests, solid plants) as their own class, so "solid" and "reproducible" stop being conflated;
- **trees versus built structures** inside class 4 (canopies must not read as floating buildings), plus a `rim`/`clipped bottom` flag pair (EC-D27 has no clipped-bottom flag today).

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
any difference (blocks, column floats, or the surface summary: ground/water top, depth, top kind), and it reports surface columns separately. In `--client-compare` mode a top-kind-only difference does not fail.

## Reproducibility of headers, commit string, weekly job

- `build.rs` watches `common/`, `world/`, `client/` and the probe's sources and writes `engine_commit` as `<sha>` for a clean tree or `<sha>+dirty` when tracked files differ from `HEAD`. Two dumps taken from different dirty trees therefore have different headers even with the same `HEAD` (the sample data is still deterministic): take dumps that must be byte-compared from a clean checkout. `client-dump` also records the server binary (`--version` line, sha256, mtime) and the probe's commit in the header's `client.server` object and prints a WARNING when the binary's commit hash disagrees with the probe's; asset hashes need no check, the scratch server runs on the probe's own `--assets`.
- **Weekly discrepancy job:** `scripts/weekly_discrepancy.sh ASSETS_DIR [LOG_DIR]` builds both binaries and runs `arena_client_check.sh`, logging to a timestamped directory, with exit 0 pass / 1 differences / 2 environment or build failure. It is meant to run **on the VPS** (which has the Git LFS assets locally), triggered over SSH with the existing `VPS_SSH_KEY` pattern (`release.yml` / `publish-docker.yml`), **not** on GitHub Actions, whose code CI deliberately never pulls LFS. No cron or timer is installed by this repository; add one on the VPS only when the arena assets are there.
- Startup is bounded: the scratch server's signal cleanup is installed before anything is spawned, `admin add` and the character-list wait have deadlines, the log is read incrementally, and a lost port race is retried once.

## Scope and caveats

- The fast path equals what the client receives for terrain and water (measured
  bit-identical over 3.09 M columns), but it passes no regional terrain overrides
  and no calendar, so sprites can differ from a live server. Always cover the
  whole declared box: sparse transects are what hid the original water bugs.
- Duplicate sampling: `World::generate_chunk` samples every column internally but
  returns only the chunk, so the probe samples each column a second time for the
  float fields (and now for the surface used by the classifier). Measured on the
  5 x 5 km box (CPU seconds summed over workers, printed by `dump`):
  generate_chunk 307 s, second sampling pass 60 s (15.5 % of CPU), block
  classification 20 s. It cannot be removed without changing the shipped `world`
  crate, which this tool does not do.
- Memory (5 x 5 km, z -154..1455, 25 M columns, peak RSS): dump 2.89 GB before
  the write-path/reader rework, 2.43 GB after; `info` (read) 1.89 GB before, 1.47
  GB after; the write step alone adds 0.28 GB over the in-memory dump (it was
  0.91 GB) and takes 0.48 s instead of 1.12 s.
- The engine commit in a dump header is `<sha>` for a clean tree and
  `<sha>+dirty` when tracked files differ from `HEAD` at the last build-script run
  (works in git worktrees; edits under `common`, `world`, `client` and this crate
  re-run it).
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
