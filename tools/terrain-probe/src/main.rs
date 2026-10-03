//! `terrain-probe`: developer tooling that samples the real generated
//! Cromatolis world at 1 m resolution and writes/reads `tprobe v1` dumps.
//! Never shipped; see `README.md`.

mod client_dump;
mod diff;
mod format;
mod legacy;
mod probe;
mod scratch_server;
#[cfg(test)] mod test_alloc;
mod tiles;

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use vek::Vec2;

use crate::{
    format::{Dump, SiteRec, flag},
    probe::{Box2, DumpOpts, Res},
};

#[cfg(test)]
#[global_allocator]
static ALLOC: test_alloc::Tracking = test_alloc::Tracking;

#[derive(Parser)]
#[command(
    name = "terrain-probe",
    about = "Sample the real Cromatolis world at 1 m and inspect `tprobe v1` dumps"
)]
struct Cli {
    /// Asset root (a directory named `assets`). Defaults to $VELOREN_ASSETS,
    /// then the engine's normal asset discovery.
    #[arg(long, global = true)]
    assets: Option<PathBuf>,
    /// World seed, as the server's `world_seed` (the research used 0).
    #[arg(long, global = true, default_value_t = 0)]
    seed: u32,
    /// Worker threads (default: all cores).
    #[arg(long, global = true)]
    threads: Option<usize>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fast path: generate every chunk of a box and write a `tprobe v1` dump.
    Dump {
        /// `x0,y0,x1,y1` in world metres, half-open.
        #[arg(long = "box")]
        bx: String,
        #[arg(long)]
        out: PathBuf,
        /// Lowest sampled z (default: automatic from the sim table).
        #[arg(long, allow_hyphen_values = true)]
        zmin: Option<i32>,
        /// One past the highest sampled z (default: automatic).
        #[arg(long, allow_hyphen_values = true)]
        zmax: Option<i32>,
        /// Sites/authored points are recorded within this many metres of the
        /// box.
        #[arg(long, default_value_t = 600)]
        margin: i32,
        /// Record sprite blocks as class 5 instead of air. The engine places
        /// sprites with a dynamic RNG, so such a dump is NOT byte-reproducible.
        #[arg(long)]
        keep_sprites: bool,
        /// Skip the pre-flight memory estimate that refuses very large boxes.
        #[arg(long)]
        force: bool,
    },
    /// Print a transect: every column along a line, from a dump.
    Cols {
        #[arg(long = "in")]
        input: PathBuf,
        /// `x0,y0,x1,y1` world metres; one column per `--step` metres.
        #[arg(long)]
        line: String,
        #[arg(long, default_value_t = 1.0)]
        step: f32,
        /// Also print each column's run-length encoding (`class:len,...`).
        #[arg(long)]
        runs: bool,
    },
    /// Per-chunk sim table (alt, water_alt, river kind/velocity/cross-section,
    /// rockiness, cliffs, ...). From a dump (`--in`) or generated live
    /// (`--box`).
    Sim {
        #[arg(long = "in", conflicts_with = "bx")]
        input: Option<PathBuf>,
        /// `x0,y0,x1,y1` world metres (live; takes ~3 s world generation, no
        /// chunk generation).
        #[arg(long = "box")]
        bx: Option<String>,
        /// Chunks of context around the box (live mode).
        #[arg(long, default_value_t = 0)]
        pad: i32,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Authored sites, landmarks and other authored points near a position.
    SitesNear {
        /// `x,y` world metres.
        #[arg(long, conflicts_with = "bx")]
        center: Option<String>,
        #[arg(long, default_value_t = 600)]
        radius: i32,
        /// Use a box (plus `--radius` as the margin) instead of a point.
        #[arg(long = "box")]
        bx: Option<String>,
        /// Skip generating the world; list only the authored `.ron` points.
        #[arg(long)]
        no_world: bool,
        /// Exit with status 1 when anything is found (arena-emptiness guard).
        #[arg(long)]
        require_empty: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print a dump's header and column statistics.
    Info {
        #[arg(long = "in")]
        input: PathBuf,
    },
    /// Client path: stream the box through a throw-away local server and a
    /// headless bot, and dump the blocks the CLIENT holds (`path = "client"`).
    ///
    /// Starts `xindeler-server-cli` on scratch userdata in the OS temp dir
    /// (loopback, free ports, no auth, Cromatolis map, `--seed`, calendar off),
    /// teleports an admin bot along a tiling of the box and waits for every
    /// chunk. Only the server process it started is ever killed.
    ClientDump {
        /// `x0,y0,x1,y1` in world metres, half-open.
        #[arg(long = "box")]
        bx: String,
        #[arg(long)]
        out: PathBuf,
        /// Lowest sampled z (default: automatic, same as `dump`).
        #[arg(long, allow_hyphen_values = true)]
        zmin: Option<i32>,
        /// One past the highest sampled z (default: automatic).
        #[arg(long, allow_hyphen_values = true)]
        zmax: Option<i32>,
        /// Sites/authored points are recorded within this many metres of the
        /// box.
        #[arg(long, default_value_t = 600)]
        margin: i32,
        /// Terrain view distance the bot requests, in chunks; it sets the tile
        /// size.
        #[arg(long, default_value_t = 24)]
        view_distance: u32,
        /// Seconds to wait for one tile's chunks before giving up on it.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Seconds without a new chunk before the bot is nudged and chunks are
        /// requested again.
        #[arg(long, default_value_t = 30)]
        stall_secs: u64,
        /// Seconds to wait for the server to finish world generation.
        #[arg(long, default_value_t = 900)]
        server_timeout: u64,
        /// Skip the pre-flight memory estimate that refuses very large boxes.
        #[arg(long)]
        force: bool,
        /// `xindeler-server-cli` binary (default: next to this executable, or
        /// $TPROBE_SERVER_BIN).
        #[arg(long)]
        server_bin: Option<PathBuf>,
        /// Keep the server log as `<out>.server.log` instead of deleting it
        /// with the scratch directory.
        #[arg(long)]
        keep_server_log: bool,
        /// Also run the fast path on the same box and z range and print the
        /// discrepancy (terrain/water must be identical outside the landing
        /// radius; structures/sprites are reported separately).
        #[arg(long)]
        compare_fast: bool,
        /// With `--compare-fast`: also write the fast dump here.
        #[arg(long, requires = "compare_fast")]
        save_fast: Option<PathBuf>,
        /// Exit 1 when fewer than this percentage of chunks streamed.
        #[arg(long, default_value_t = 99.9)]
        min_streamed: f64,
        /// Metres around each bot landing point where a terrain difference is
        /// attributed to the bot itself.
        #[arg(long, default_value_t = 8)]
        landing_radius: i32,
        /// Print this many example differences.
        #[arg(long, default_value_t = 10)]
        show: usize,
    },
    /// Compare two dumps of the same box and z range block by block.
    Diff {
        a: PathBuf,
        b: PathBuf,
        /// Print this many example differences.
        #[arg(long, default_value_t = 10)]
        show: usize,
        /// Fast-vs-client comparison: ignore the column floats (a client dump
        /// has none), tolerate terrain differences within `--landing-radius`
        /// of a recorded bot landing, and report structure/sprite-only
        /// differences without failing on them.
        #[arg(long)]
        client_compare: bool,
        #[arg(long, default_value_t = 8)]
        landing_radius: i32,
    },
    /// Compare a dump with a legacy research dump
    /// (`blocks.bin`/`cols.f32`/`meta.txt`).
    CheckLegacy {
        #[arg(long = "in")]
        input: PathBuf,
        /// Directory holding the legacy `blocks.bin`, `cols.f32`, `meta.txt`.
        #[arg(long)]
        legacy: PathBuf,
    },
    /// Write authored water raster tiles and their manifest (synthetic
    /// scenarios or imported rasters) into an asset root's `world/map`, so
    /// the engine's authored water layer can be exercised before the real
    /// exporter exists. Validates the result with the engine's own loader.
    AwWrite {
        /// JSON spec: `{"stem": "cromatolis_v0", "regions": [RegionSpec, ...]}`
        /// (see `world::authored_raster::writer`); relative raster paths are
        /// resolved against the spec's directory.
        #[arg(long)]
        spec: PathBuf,
        /// Output directory (normally `<scratch assets>/world/map`).
        #[arg(long)]
        map_dir: PathBuf,
    },
}

/// Select the asset root before any engine code reads it.
fn select_assets(cli: &Cli) -> Res<()> {
    if let Some(dir) = &cli.assets {
        let dir = dir
            .canonicalize()
            .map_err(|e| format!("--assets {dir:?}: {e}"))?;
        // SAFETY: called from `main` before any thread is spawned.
        unsafe { std::env::set_var("VELOREN_ASSETS", &dir) };
    }
    Ok(())
}

fn check_assets_root(root: &Path) -> Res<()> {
    if !root.join("world/map/cromatolis_v0.bin").exists() {
        return Err(format!(
            "{} has no world/map/cromatolis_v0.bin: the asset root must be a directory named \
             `assets` (a scratch root with another name silently falls back to the repo assets) \
             and contain the Cromatolis map",
            root.display()
        )
        .into());
    }
    Ok(())
}

fn load_probe(cli: &Cli) -> Res<probe::Probe> {
    let t = std::time::Instant::now();
    let p = probe::load(cli.seed, cli.threads)?;
    check_assets_root(&p.assets_root)?;
    eprintln!(
        "world generated in {:.1}s (seed {}, assets {})",
        t.elapsed().as_secs_f32(),
        cli.seed,
        p.assets_root.display()
    );
    Ok(p)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        },
    }
}

fn run(cli: &Cli) -> Res<ExitCode> {
    select_assets(cli)?;
    match &cli.cmd {
        Cmd::Dump {
            bx,
            out,
            zmin,
            zmax,
            margin,
            keep_sprites,
            force,
        } => {
            let bx = Box2::parse(bx)?;
            let p = load_probe(cli)?;
            let opts = DumpOpts {
                bx,
                zmin: *zmin,
                zmax: *zmax,
                site_margin: *margin,
                keep_sprites: *keep_sprites,
                force: *force,
            };
            let last = std::sync::atomic::AtomicUsize::new(0);
            let (dump, stats) = probe::dump(&p, &opts, &|done, total| {
                let pct = done * 100 / total.max(1);
                let tenth = pct / 10;
                if tenth > last.swap(tenth, std::sync::atomic::Ordering::Relaxed) {
                    eprintln!("  {done}/{total} chunks ({pct}%)");
                }
            })?;
            let t = std::time::Instant::now();
            dump.write_file(out)?;
            let size = std::fs::metadata(out)?.len();
            eprintln!(
                "dumped {} chunks / {} columns in {:.1}s ({:.0} chunks/min), wrote {} ({:.1} MB) \
                 in {:.1}s; z {}..{}, clipped columns: {}",
                stats.chunks,
                stats.columns,
                stats.gen_secs,
                stats.chunks as f32 / stats.gen_secs * 60.0,
                out.display(),
                size as f64 / 1e6,
                t.elapsed().as_secs_f32(),
                dump.header.zmin,
                dump.header.zmax,
                dump.header.stats["clipped_top_columns"],
            );
            eprintln!(
                "cpu seconds over all workers: generate_chunk {:.1}, column re-sampling {:.1}, \
                 block classification {:.1}",
                stats.cpu_gen_secs, stats.cpu_sample_secs, stats.cpu_runs_secs
            );
            Ok(ExitCode::SUCCESS)
        },
        Cmd::ClientDump {
            bx,
            out,
            zmin,
            zmax,
            margin,
            view_distance,
            timeout,
            stall_secs,
            server_timeout,
            force,
            server_bin,
            keep_server_log,
            compare_fast,
            save_fast,
            min_streamed,
            landing_radius,
            show,
        } => {
            let bx = Box2::parse(bx)?;
            let p = load_probe(cli)?;
            let opts = client_dump::ClientDumpOpts {
                bx,
                zmin: *zmin,
                zmax: *zmax,
                site_margin: *margin,
                view_distance: *view_distance,
                tile_timeout: std::time::Duration::from_secs(*timeout),
                stall_secs: *stall_secs,
                server_bin: server_bin
                    .clone()
                    .unwrap_or_else(scratch_server::default_server_bin),
                server_ready_timeout: std::time::Duration::from_secs(*server_timeout),
                force: *force,
                keep_server_log: keep_server_log.then(|| {
                    let mut s = out.clone().into_os_string();
                    s.push(".server.log");
                    PathBuf::from(s)
                }),
            };
            let t0 = std::time::Instant::now();
            let res = client_dump::run(&p, &opts)?;
            let dump = res.dump;
            dump.write_file(out)?;
            let ci = dump.header.client.as_ref().ok_or("client info missing")?;
            eprintln!(
                "client dump: {}/{} chunks streamed ({:.3}%), {} missing, {} tile(s); server \
                 start {:.0}s, streaming {:.0}s, total {:.0}s; wrote {} ({:.1} MB), z {}..{}",
                ci.chunks_streamed,
                ci.chunks_total,
                ci.streamed_pct(),
                ci.missing_chunks.len(),
                ci.tiles.len(),
                res.server_secs,
                res.stream_secs,
                t0.elapsed().as_secs_f32(),
                out.display(),
                std::fs::metadata(out)?.len() as f64 / 1e6,
                dump.header.zmin,
                dump.header.zmax,
            );
            if !ci.missing_chunks.is_empty() {
                eprintln!(
                    "missing chunks (first 20): {:?}",
                    &ci.missing_chunks[..ci.missing_chunks.len().min(20)]
                );
            }
            let mut ok = ci.streamed_pct() >= *min_streamed;
            if !ok {
                eprintln!(
                    "FAIL: {:.3}% streamed, below --min-streamed {min_streamed}",
                    ci.streamed_pct()
                );
            }
            if *compare_fast {
                let fopts = DumpOpts {
                    bx,
                    zmin: Some(dump.header.zmin),
                    zmax: Some(dump.header.zmax),
                    site_margin: *margin,
                    keep_sprites: false,
                    force: true,
                };
                let (fast, st) = probe::dump(&p, &fopts, &|_, _| {})?;
                eprintln!("fast path: {} chunks in {:.1}s", st.chunks, st.gen_secs);
                if let Some(path) = save_fast {
                    fast.write_file(path)?;
                }
                let dopts = diff::DiffOpts {
                    show: *show,
                    client_compare: true,
                    landing_radius: *landing_radius,
                };
                let rep = diff::compare_with(&fast, &dump, &dopts)?;
                println!("== fast path vs client path ==");
                rep.print(&dopts);
                let d_ok = rep.ok(&dopts);
                println!(
                    "discrepancy: {}",
                    if d_ok {
                        "PASS (no terrain/water difference outside the landing radius)"
                    } else {
                        "FAIL"
                    }
                );
                ok &= d_ok;
            }
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        },
        Cmd::Cols {
            input,
            line,
            step,
            runs,
        } => {
            let d = Dump::read_file(input)?;
            print_cols(&d, line, *step, *runs)?;
            Ok(ExitCode::SUCCESS)
        },
        Cmd::Sim {
            input,
            bx,
            pad,
            out,
        } => {
            let text = match (input, bx) {
                (Some(i), None) => Dump::read_file(i)?.sim_csv,
                (None, Some(b)) => {
                    let b = Box2::parse(b)?;
                    let p = load_probe(cli)?;
                    let (c0, c1) = b.chunk_range();
                    probe::sim_table(&p, c0 - Vec2::broadcast(*pad), c1 + Vec2::broadcast(*pad))
                },
                _ => return Err("give exactly one of --in or --box".into()),
            };
            match out {
                Some(path) => std::fs::write(path, text)?,
                None => std::io::stdout().write_all(text.as_bytes())?,
            }
            Ok(ExitCode::SUCCESS)
        },
        Cmd::SitesNear {
            center,
            radius,
            bx,
            no_world,
            require_empty,
            json,
        } => {
            let b = match (center, bx) {
                (Some(c), None) => {
                    let v: Vec<i32> = c
                        .split(',')
                        .map(|s| s.trim().parse::<i32>())
                        .collect::<Result<_, _>>()?;
                    let [x, y] = v[..] else {
                        return Err("--center needs x,y".into());
                    };
                    let (x1, y1) = x
                        .checked_add(1)
                        .zip(y.checked_add(1))
                        .ok_or("--center is at the i32 limit")?;
                    Box2 {
                        x0: x,
                        y0: y,
                        x1,
                        y1,
                    }
                },
                (None, Some(b)) => Box2::parse(b)?,
                _ => return Err("give exactly one of --center or --box".into()),
            };
            let mut found: Vec<SiteRec> = Vec::new();
            if *no_world {
                // Authored points only: needs the asset root but not the world.
                let root = std::env::var_os("VELOREN_ASSETS")
                    .map(PathBuf::from)
                    .ok_or("--no-world needs --assets or $VELOREN_ASSETS")?;
                check_assets_root(&root)?;
                // The world is always square-sized in the shipped map.
                found.extend(probe::authored_near(
                    &root,
                    Vec2::broadcast(32768),
                    b,
                    *radius,
                )?);
            } else {
                let p = load_probe(cli)?;
                found.extend(probe::world_sites(&p.index.as_index_ref(), b, *radius));
                found.extend(probe::authored_near(
                    &p.assets_root,
                    probe::world_size(p.world.sim()),
                    b,
                    *radius,
                )?);
            }
            let (cx, cy) = (
                (i64::from(b.x0) + i64::from(b.x1)) / 2,
                (i64::from(b.y0) + i64::from(b.y1)) / 2,
            );
            found.sort_by_key(|r| {
                let (dx, dy) = (i64::from(r.wx) - cx, i64::from(r.wy) - cy);
                dx * dx + dy * dy
            });
            if *json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else {
                println!("source\tid\tname\tkind\twx\twy\tradius\tdist_to_centre");
                for r in &found {
                    let d = ((r.wx as f64 - cx as f64).hypot(r.wy as f64 - cy as f64)).round();
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{d}",
                        r.source,
                        r.id.as_deref().unwrap_or("-"),
                        r.name.as_deref().unwrap_or("-"),
                        r.kind.as_deref().unwrap_or("-"),
                        r.wx,
                        r.wy,
                        r.radius.map_or("-".to_string(), |v| format!("{v:.0}")),
                    );
                }
                eprintln!("{} entries within {radius} m", found.len());
            }
            Ok(if *require_empty && !found.is_empty() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        },
        Cmd::Info { input } => {
            let d = Dump::read_file(input)?;
            print_info(&d);
            Ok(ExitCode::SUCCESS)
        },
        Cmd::Diff {
            a,
            b,
            show,
            client_compare,
            landing_radius,
        } => {
            let opts = diff::DiffOpts {
                show: *show,
                client_compare: *client_compare,
                landing_radius: *landing_radius,
            };
            let rep = diff::compare_with(&Dump::read_file(a)?, &Dump::read_file(b)?, &opts)?;
            rep.print(&opts);
            let ok = rep.ok(&opts);
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        },
        Cmd::AwWrite { spec, map_dir } => aw_write(spec, map_dir),
        Cmd::CheckLegacy { input, legacy } => {
            let d = Dump::read_file(input)?;
            let ok = legacy::compare(&d, legacy)?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        },
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AwSpec {
    stem: String,
    regions: Vec<world::authored_raster::writer::RegionSpec>,
}

fn aw_write(spec_path: &Path, map_dir: &Path) -> Res<ExitCode> {
    use world::authored_raster::{
        AuthoredRasters,
        writer::{self, PaintOp},
    };
    let text =
        std::fs::read_to_string(spec_path).map_err(|e| format!("{}: {e}", spec_path.display()))?;
    let mut spec: AwSpec =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", spec_path.display()))?;
    let base = spec_path.parent().unwrap_or(Path::new("."));
    for region in &mut spec.regions {
        for op in &mut region.ops {
            if let PaintOp::Raster {
                surface_file,
                bed_file,
                ..
            } = op
            {
                *surface_file = base.join(&*surface_file);
                *bed_file = base.join(&*bed_file);
            }
        }
    }
    let built = spec
        .regions
        .iter()
        .map(writer::build_region)
        .collect::<Result<Vec<_>, _>>()?;
    // Validate with the engine's own loader before writing anything (the
    // map size only matters for the rim check; Cromatolis is 32768 m).
    let files: std::collections::HashMap<(String, i32, i32), Vec<u8>> = built
        .iter()
        .flat_map(|r| {
            r.tiles
                .iter()
                .map(|((tx, ty), b)| ((r.manifest.id.clone(), *tx, *ty), b.clone()))
        })
        .collect();
    let fetch = |id: &str, _: world::authored_raster::format::LayerKind, tx: i32, ty: i32| {
        files
            .get(&(id.to_string(), tx, ty))
            .cloned()
            .ok_or_else(|| format!("missing tile {id} {tx} {ty}"))
    };
    let loaded =
        AuthoredRasters::from_manifest(writer::manifest(&built), Vec2::broadcast(32768), &fetch)
            .map_err(|e| e.to_string())?;
    let written = writer::write_assets(map_dir, &spec.stem, &built)?;
    for (id, b) in loaded.regions() {
        let r = built.iter().find(|r| r.manifest.id == id).expect("built");
        eprintln!(
            "region {id}: box {:?}..{:?}, {} tile(s)",
            b.min,
            b.max,
            r.tiles.len()
        );
    }
    for p in written {
        println!("{}", p.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn flag_letters(f: u8) -> String {
    [
        (flag::STRUCTURE, 'S'),
        (flag::SPRITE, 'p'),
        (flag::VOID, 'V'),
        (flag::LIQUID, 'W'),
        (flag::CLIPPED_TOP, 'C'),
        (flag::STRUCTURE_ABOVE_GROUND, 'A'),
        (flag::LAVA, 'L'),
        (flag::RIM_NO_SAMPLE, 'R'),
    ]
    .iter()
    .map(|&(b, c)| if f & b != 0 { c } else { '.' })
    .collect()
}

fn print_cols(d: &Dump, line: &str, step: f32, runs: bool) -> Res<()> {
    let v: Vec<f32> = line
        .split(',')
        .map(|s| s.trim().parse::<f32>())
        .collect::<Result<_, _>>()?;
    let [x0, y0, x1, y1] = v[..] else {
        return Err("--line needs x0,y0,x1,y1".into());
    };
    if step <= 0.0 {
        return Err("--step must be positive".into());
    }
    let kind_name: std::collections::BTreeMap<u8, &str> = d
        .header
        .block_kinds
        .iter()
        .map(|(k, &c)| (c, k.as_str()))
        .collect();
    let off = d.run_offsets();
    let len = (x1 - x0).hypot(y1 - y0);
    let n = (len / step).floor() as usize + 1;
    println!(
        "i\tx\ty\talt\triverless_alt\twater_level\tground_top\ttop_kind\twater_top\tliquid_depth\\
         tflags(SpVWCALR)\truns"
    );
    let mut last = None;
    for i in 0..n {
        let t = if len == 0.0 {
            0.0
        } else {
            i as f32 * step / len
        };
        let (x, y) = (
            (x0 + (x1 - x0) * t).round() as i32,
            (y0 + (y1 - y0) * t).round() as i32,
        );
        if last == Some((x, y)) {
            continue;
        }
        last = Some((x, y));
        let Some(c) = d.col_index(x, y) else {
            println!("{i}\t{x}\t{y}\t(outside the dump box)");
            continue;
        };
        let z = |v: i16| {
            if v == format::NO_Z {
                "-".to_string()
            } else {
                v.to_string()
            }
        };
        let kind = if d.top_kind[c] == 255 {
            "-"
        } else {
            kind_name.get(&d.top_kind[c]).copied().unwrap_or("?")
        };
        let rs = if runs {
            d.runs_at(&off, c)
                .map(|(cl, n)| format!("{cl}:{n}"))
                .collect::<Vec<_>>()
                .join(",")
        } else {
            d.run_counts[c].to_string()
        };
        println!(
            "{i}\t{x}\t{y}\t{:.2}\t{:.2}\t{:.2}\t{}\t{kind}\t{}\t{}\t{}\t{rs}",
            d.alt[c],
            d.riverless_alt[c],
            d.water_level[c],
            z(d.ground_top[c]),
            z(d.water_top[c]),
            d.liquid_depth[c],
            flag_letters(d.flags[c]),
        );
    }
    Ok(())
}

fn print_info(d: &Dump) {
    let h = &d.header;
    println!("format      {}", h.format);
    println!("path        {}", h.path);
    println!("box         {:?} ({} x {} m)", h.box_xy, h.nx, h.ny);
    println!("z           {}..{} ({} blocks)", h.zmin, h.zmax, h.height());
    println!("seed        {}", h.seed);
    println!("calendar    {:?}", h.calendar);
    println!("commit      {}", h.engine_commit);
    println!("stats       {:?}", h.stats);
    println!("sites       {}", d.sites.len());
    for (k, v) in &h.assets {
        println!("asset       {k} {}", &v[..16.min(v.len())]);
    }
    let n = d.alt.len();
    let count = |f: u8| d.flags.iter().filter(|&&x| x & f != 0).count();
    println!(
        "columns     {n}; with water {}, structures {}, sprites {}, voids {}, clipped {}, \
         structure above ground {}, lava {}, rim (no sample) {}",
        count(flag::LIQUID),
        count(flag::STRUCTURE),
        count(flag::SPRITE),
        count(flag::VOID),
        count(flag::CLIPPED_TOP),
        count(flag::STRUCTURE_ABOVE_GROUND),
        count(flag::LAVA),
        count(flag::RIM_NO_SAMPLE)
    );
    let tops: Vec<i16> = d
        .ground_top
        .iter()
        .copied()
        .filter(|&z| z != format::NO_Z)
        .collect();
    if let (Some(lo), Some(hi)) = (tops.iter().min(), tops.iter().max()) {
        println!("ground_top  {lo}..{hi}");
    }
    for s in &h.sections {
        println!(
            "section     {:<14} {:>12} -> {:>12} bytes",
            s.name, s.raw_len, s.comp_len
        );
    }
}
