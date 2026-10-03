//! Client path: what a real client holds after streaming the box from a
//! throw-away server. A headless admin bot is teleported along a tiling of the
//! box; each tile's chunks are harvested once they have all arrived (the client
//! drops chunks far from its player, so a big box never sits in it at once) and
//! written to the same `tprobe v1` format as the fast path, with
//! `path = "client"`.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use common::{ViewDistances, clock::Clock, comp, terrain::TerrainChunk, vol::ReadVol};
use tokio::runtime::Runtime;
use vek::{Vec2, Vec3};
use xindeler_client::{Client, ClientType, addr::ConnectionArgs};

use crate::{
    format::{self, ClientInfo, Dump, Header, TileInfo, class, flag},
    probe::{self, Box2, CHUNK, Probe, Res},
    scratch_server::{self, BOT_USER, ScratchServer, ServerOpts},
    tiles::{self, ChunkRect},
};

pub struct ClientDumpOpts {
    pub bx: Box2,
    pub zmin: Option<i32>,
    pub zmax: Option<i32>,
    pub site_margin: i32,
    pub view_distance: u32,
    /// Per-tile limit for the chunks to arrive.
    pub tile_timeout: Duration,
    /// Seconds without any new chunk before the bot is nudged and chunks are
    /// requested again.
    pub stall_secs: u64,
    pub server_bin: PathBuf,
    pub server_ready_timeout: Duration,
    pub keep_server_log: Option<PathBuf>,
}

pub struct ClientDumpResult {
    pub dump: Dump,
    pub server_secs: f32,
    pub stream_secs: f32,
}

/// Per-column data harvested from the client.
struct Col {
    top_kind: u8,
    sum: format::ColSummary,
    runs: Vec<(u8, u16)>,
}

fn env_err<T: std::fmt::Debug>(what: &str, e: T) -> String { format!("{what}: {e:?}") }

/// Harvest the in-box columns of `chunk` (None = never streamed).
fn harvest_chunk(
    chunk: Option<&TerrainChunk>,
    b: Box2,
    key: Vec2<i32>,
    zmin: i32,
    zmax: i32,
    out: &mut [Option<Col>],
) {
    let base = key * CHUNK;
    let nx = b.nx();
    let (lx0, lx1) = (
        (b.x0 - base.x).clamp(0, CHUNK),
        (b.x1 - base.x).clamp(0, CHUNK),
    );
    let (ly0, ly1) = (
        (b.y0 - base.y).clamp(0, CHUNK),
        (b.y1 - base.y).clamp(0, CHUNK),
    );
    for ly in ly0..ly1 {
        for lx in lx0..lx1 {
            let rel = Vec2::new(lx, ly);
            let w = base + rel;
            let runs = match chunk {
                Some(ch) => probe::column_runs(ch, rel, zmin, zmax, false),
                None => vec![(class::UNLOADED, (zmax - zmin) as u16)],
            };
            let sum = format::summarize(zmin, &runs);
            let top_kind = match chunk {
                Some(ch) if sum.ground_top != format::NO_Z => ch
                    .get(rel.with_z(i32::from(sum.ground_top)))
                    .map_or(255, |bl| bl.kind() as u8),
                _ => 255,
            };
            let idx = (w.y - b.y0) as usize * nx + (w.x - b.x0) as usize;
            out[idx] = Some(Col {
                top_kind,
                sum,
                runs,
            });
        }
    }
}

/// Fixed-rate client ticking plus the last few chat lines (admin command
/// errors show up there).
struct Driver {
    clock: Clock,
    chat: Vec<String>,
}

impl Driver {
    fn new() -> Self {
        Self {
            clock: Clock::new(Duration::from_secs_f32(1.0 / 30.0)),
            chat: Vec::new(),
        }
    }

    fn tick(&mut self, c: &mut Client) -> Res<()> {
        self.clock.tick();
        let evs = c
            .tick(comp::ControllerInputs::default(), self.clock.real_dt())
            .map_err(|e| env_err("client tick", e))?;
        for e in evs {
            if let xindeler_client::Event::Chat(m) = e {
                self.chat.push(format!("{m:?}"));
                if self.chat.len() > 8 {
                    self.chat.remove(0);
                }
            }
        }
        Ok(())
    }
}

/// Connect, create a character, return a client in the world.
fn connect_bot(
    runtime: &Arc<Runtime>,
    port: u16,
    view_distance: u32,
    drv: &mut Driver,
) -> Res<Client> {
    let t0 = Instant::now();
    let mut client = loop {
        let r = runtime.block_on(Client::new(
            ConnectionArgs::Tcp {
                prefer_ipv6: false,
                hostname: format!("127.0.0.1:{port}"),
            },
            Arc::clone(runtime),
            &mut None,
            BOT_USER,
            "",
            None,
            |_| false,
            || None,
            None,
            &|_| {},
            |_| {},
            PathBuf::new(),
            ClientType::Game,
        ));
        match r {
            Ok(c) => break c,
            Err(e) if t0.elapsed() < Duration::from_secs(60) => {
                eprintln!("connect retry: {e:?}");
                std::thread::sleep(Duration::from_secs(2));
            },
            Err(e) => return Err(env_err("connect", e).into()),
        }
    };
    client.load_character_list();
    while client.character_list().loading {
        drv.tick(&mut client)?;
    }
    let chosen = client.possible_starting_sites().first().copied();
    let alias = format!("Probe{}", std::process::id() % 1000);
    client.create_character(
        alias.clone(),
        Some("common.items.weapons.sword.starter".into()),
        None,
        comp::body::humanoid::Body {
            species: comp::body::humanoid::Species::Human,
            body_type: comp::body::humanoid::BodyType::Male,
            hair_style: 0,
            beard: 0,
            eyes: 0,
            accessory: 0,
            hair_color: 0,
            skin: 0,
            eye_color: 0,
            height_scale: 0,
        }
        .into(),
        false,
        chosen,
        comp::class::ClassKind::Warrior,
        comp::Ethos::default(),
        comp::Background::default(),
    );
    client.load_character_list();
    let t1 = Instant::now();
    let id = loop {
        drv.tick(&mut client)?;
        if let Some(id) = client
            .character_list()
            .characters
            .iter()
            .find(|c| c.character.alias == alias)
            .and_then(|c| c.character.id)
        {
            break id;
        }
        if t1.elapsed() > Duration::from_secs(60) {
            return Err("character creation timed out".into());
        }
    };
    client.request_character(id, ViewDistances {
        terrain: view_distance,
        entity: 4,
    });
    let mut n = 0;
    while client.position().is_none() || n < 60 {
        drv.tick(&mut client)?;
        n += 1;
        if t1.elapsed() > Duration::from_secs(120) {
            return Err("bot never entered the world".into());
        }
    }
    Ok(client)
}

fn count_loaded(client: &Client, t: ChunkRect) -> usize {
    let terr = client.state().terrain();
    t.keys()
        .filter(|&(cx, cy)| terr.get_key_real(Vec2::new(cx, cy)).is_some())
        .count()
}

pub fn run(p: &Probe, o: &ClientDumpOpts) -> Res<ClientDumpResult> {
    let b = o.bx;
    let size = probe::world_size(p.world.sim());
    if b.x0 < 0 || b.y0 < 0 || b.x1 > size.x || b.y1 > size.y {
        return Err(format!("box outside the world (0,0)..({},{})", size.x, size.y).into());
    }
    let (auto_lo, auto_hi) = probe::auto_z_range(p.world.sim(), b);
    let (zmin, zmax) = (o.zmin.unwrap_or(auto_lo), o.zmax.unwrap_or(auto_hi));
    if zmax <= zmin || zmax - zmin > i32::from(i16::MAX) {
        return Err(format!("bad z range {zmin}..{zmax} (height must be 1..=32767)").into());
    }
    let (c0, c1) = b.chunk_range();
    let all = ChunkRect {
        cx0: c0.x,
        cy0: c0.y,
        cx1: c1.x,
        cy1: c1.y,
    };
    let plan = tiles::plan_tiles(all, o.view_distance)?;
    eprintln!(
        "{} chunks in {} tile(s) of up to {}x{} chunks at view distance {}",
        all.count(),
        plan.len(),
        plan.iter().map(ChunkRect::width).max().unwrap_or(0),
        plan.iter().map(ChunkRect::height).max().unwrap_or(0),
        o.view_distance
    );

    let t_server = Instant::now();
    let mut server = ScratchServer::start(&ServerOpts {
        server_bin: o.server_bin.clone(),
        assets: p.assets_root.clone(),
        seed: p.seed,
        view_distance: o.view_distance,
        ready_timeout: o.server_ready_timeout,
        keep_log_to: o.keep_server_log.clone(),
    })?;
    scratch_server::install_signal_cleanup(Arc::clone(&server.inner));
    let server_secs = t_server.elapsed().as_secs_f32();

    let runtime = Arc::new(Runtime::new()?);
    let mut drv = Driver::new();
    let mut client = connect_bot(&runtime, server.port, o.view_distance, &mut drv)?;

    let t_stream = Instant::now();
    let n = b.nx() * b.ny();
    let mut cols: Vec<Option<Col>> = (0..n).map(|_| None).collect();
    let mut infos = Vec::with_capacity(plan.len());
    let mut missing: Vec<[i32; 2]> = Vec::new();
    let mut streamed_total = 0u64;
    for (ti, tile) in plan.iter().enumerate() {
        let t_tile = Instant::now();
        let (gx, gy) = tile.center_wpos();
        // A few blocks above the base altitude: close enough that landing is
        // harmless, high enough not to start inside the ground.
        let alt = p
            .world
            .sim()
            .get(Vec2::new(gx, gy).map(|e| e.div_euclid(CHUNK)))
            .map_or(100.0, |c| c.alt);
        let gz = alt.ceil() as i32 + 3;
        let send_goto = |c: &mut Client, dx: i32| {
            c.send_command("goto".to_owned(), vec![
                (gx + dx).to_string(),
                gy.to_string(),
                gz.to_string(),
            ]);
        };
        send_goto(&mut client, 0);
        let total = tile.count();
        let (mut loaded, mut best, mut last_progress) = (0usize, 0usize, Instant::now());
        let (mut retries, mut last_log, mut at_target) = (0i32, Instant::now(), false);
        loop {
            drv.tick(&mut client)?;
            if let Some(status) = server.exited()? {
                return Err(format!(
                    "server died ({status}) during tile {}; log tail:\n{}",
                    ti + 1,
                    server.log_tail(25)
                )
                .into());
            }
            if client.is_dead() {
                client.respawn();
                send_goto(&mut client, 0);
            }
            if let Some(pos) = client.position() {
                let d = Vec2::new(pos.x - gx as f32, pos.y - gy as f32).magnitude();
                if d < 24.0 {
                    at_target = true;
                }
            }
            if at_target {
                loaded = count_loaded(&client, *tile);
                if loaded > best {
                    best = loaded;
                    last_progress = Instant::now();
                }
                if loaded == total {
                    break;
                }
            }
            if t_tile.elapsed() > o.tile_timeout {
                eprintln!(
                    "tile {}/{}: timeout after {:.0}s with {loaded}/{total} chunks",
                    ti + 1,
                    plan.len(),
                    t_tile.elapsed().as_secs_f32()
                );
                break;
            }
            if last_progress.elapsed() > Duration::from_secs(o.stall_secs.max(1)) {
                retries += 1;
                eprintln!(
                    "tile {}/{}: stalled at {loaded}/{total} (position {:?}), retry {retries}: \
                     chat {:?}",
                    ti + 1,
                    plan.len(),
                    client.position(),
                    drv.chat.last()
                );
                at_target = false;
                send_goto(&mut client, if retries % 2 == 0 { 0 } else { 1 });
                last_progress = Instant::now();
            }
            if last_log.elapsed() > Duration::from_secs(5) {
                eprintln!(
                    "  tile {}/{}: {loaded}/{total} chunks, position {:?}",
                    ti + 1,
                    plan.len(),
                    client.position().map(|v| v.map(|e| e as i32))
                );
                last_log = Instant::now();
            }
        }
        // Harvest this tile before moving on (the client forgets far chunks).
        let landing = client.position().map(|v: Vec3<f32>| [v.x, v.y, v.z]);
        let mut got = 0u64;
        {
            let terr = client.state().terrain();
            for (cx, cy) in tile.keys() {
                let key = Vec2::new(cx, cy);
                let ch: Option<&TerrainChunk> = terr.get_key_real(key);
                if ch.is_some() {
                    got += 1;
                } else {
                    missing.push([cx, cy]);
                }
                harvest_chunk(ch, b, key, zmin, zmax, &mut cols);
            }
        }
        streamed_total += got;
        infos.push(TileInfo {
            chunks: [tile.cx0, tile.cy0, tile.cx1, tile.cy1],
            goto: [gx, gy, gz],
            landing,
            streamed: got,
            total: total as u64,
            secs: t_tile.elapsed().as_secs_f32(),
        });
        eprintln!(
            "tile {}/{} done: {got}/{total} chunks in {:.1}s, bot at {:?}",
            ti + 1,
            plan.len(),
            t_tile.elapsed().as_secs_f32(),
            landing
        );
    }
    let stream_secs = t_stream.elapsed().as_secs_f32();
    drop(client);
    drop(server);

    // Flatten into the dump's column arrays.
    let height = (zmax - zmin) as u16;
    let mut d = Dump {
        header: Header {
            format: format::FORMAT_NAME.into(),
            box_xy: [b.x0, b.y0, b.x1, b.y1],
            zmin,
            zmax,
            nx: b.nx() as u32,
            ny: b.ny() as u32,
            seed: p.seed,
            path: "client".into(),
            calendar: None,
            engine_commit: env!("TPROBE_ENGINE_COMMIT").to_string(),
            assets: probe::asset_hashes(&p.assets_root),
            class_codes: Header::class_codes(),
            block_kinds: probe::block_kind_codes(),
            stats: Default::default(),
            client: Some(ClientInfo {
                view_distance: o.view_distance,
                chunks_total: all.count() as u64,
                chunks_streamed: streamed_total,
                missing_chunks: missing,
                tiles: infos,
            }),
            sections: vec![],
        },
        // The client never receives the sampler's float fields.
        alt: vec![f32::NAN; n],
        riverless_alt: vec![f32::NAN; n],
        water_level: vec![f32::NAN; n],
        warp_factor: vec![f32::NAN; n],
        top_kind: Vec::with_capacity(n),
        flags: Vec::with_capacity(n),
        ground_top: Vec::with_capacity(n),
        water_top: Vec::with_capacity(n),
        liquid_depth: Vec::with_capacity(n),
        run_counts: Vec::with_capacity(n),
        run_class: Vec::new(),
        run_len: Vec::new(),
        sim_csv: String::new(),
        sites: Vec::new(),
    };
    let mut clipped = 0u64;
    for c in cols {
        let c = c.unwrap_or_else(|| {
            let runs = vec![(class::UNLOADED, height)];
            Col {
                top_kind: 255,
                sum: format::summarize(zmin, &runs),
                runs,
            }
        });
        d.top_kind.push(c.top_kind);
        d.flags.push(c.sum.flags);
        clipped += u64::from(c.sum.flags & flag::CLIPPED_TOP != 0);
        d.ground_top.push(c.sum.ground_top);
        d.water_top.push(c.sum.water_top);
        d.liquid_depth.push(c.sum.liquid_depth);
        d.run_counts.push(c.runs.len() as u16);
        for (cl, len) in c.runs {
            d.run_class.push(cl);
            d.run_len.push(len);
        }
    }
    // Sim table and sites come from the in-process world (same seed, assets).
    d.sim_csv = probe::sim_table(p, c0 - Vec2::broadcast(2), c1 + Vec2::broadcast(2));
    d.sites = probe::world_sites(&p.index.as_index_ref(), b, o.site_margin);
    d.sites.extend(probe::authored_points(
        &p.assets_root,
        size,
        b,
        o.site_margin,
    ));
    d.header.stats = [
        ("columns", n as u64),
        ("chunks", all.count() as u64),
        ("clipped_top_columns", clipped),
        ("sprites_kept", 0),
        ("sites", d.sites.len() as u64),
        ("floats_present", 0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    Ok(ClientDumpResult {
        dump: d,
        server_secs,
        stream_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::{DiffOpts, compare_with},
        probe::DumpOpts,
    };

    /// Real-asset end-to-end test: starts a throw-away `xindeler-server-cli`,
    /// streams a 128 m box through the headless bot and checks that the client
    /// holds exactly what the fast path generates for terrain and water.
    ///
    /// `VELOREN_ASSETS=<assets with the Cromatolis map> cargo build -p
    /// xindeler-server-cli && cargo test -p xindeler-terrain-probe -- --ignored
    /// client_dump` (the server binary is looked up next to the test
    /// executable's `deps/` dir, or in `$TPROBE_SERVER_BIN`).
    #[test]
    #[ignore = "needs VELOREN_ASSETS (Cromatolis) and a built xindeler-server-cli"]
    fn client_dump_matches_fast_path_on_a_small_box() {
        let p = probe::load(0, None).expect("world generation");
        let bx = Box2::parse("22752,24576,22880,24704").unwrap();
        let mut server_bin = scratch_server::default_server_bin();
        if !server_bin.exists() {
            // `cargo test` runs from target/<profile>/deps.
            server_bin = std::env::current_exe()
                .unwrap()
                .parent()
                .and_then(std::path::Path::parent)
                .unwrap()
                .join(format!(
                    "xindeler-server-cli{}",
                    std::env::consts::EXE_SUFFIX
                ));
        }
        let res = run(&p, &ClientDumpOpts {
            bx,
            zmin: Some(100),
            zmax: Some(300),
            site_margin: 600,
            view_distance: 24,
            tile_timeout: Duration::from_secs(300),
            stall_secs: 30,
            server_bin,
            server_ready_timeout: Duration::from_secs(600),
            keep_server_log: None,
        })
        .expect("client dump");
        let ci = res.dump.header.client.as_ref().unwrap();
        assert_eq!(ci.chunks_total, 16);
        assert_eq!(ci.chunks_streamed, 16, "missing {:?}", ci.missing_chunks);
        assert_eq!(res.dump.header.path, "client");
        let (fast, _) = probe::dump(
            &p,
            &DumpOpts {
                bx,
                zmin: Some(100),
                zmax: Some(300),
                site_margin: 600,
                keep_sprites: false,
            },
            &|_, _| {},
        )
        .unwrap();
        let opts = DiffOpts {
            show: 5,
            client_compare: true,
            landing_radius: 8,
        };
        let rep = compare_with(&fast, &res.dump, &opts).unwrap();
        rep.print(&opts);
        assert!(
            rep.ok(&opts),
            "terrain/water differs outside the landing radius"
        );
    }
}
