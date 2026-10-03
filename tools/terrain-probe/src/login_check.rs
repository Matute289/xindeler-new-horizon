//! Live login check over authored water: a character whose saved position
//! (its waypoint) is inside an authored channel must come back on dry ground.
//!
//! Starts a throw-away server (same scratch harness as `client-dump`), puts an
//! admin bot in the water at `at`, saves the waypoint there, leaves to the
//! character list and selects the character again, which loads it from the
//! database and runs the server's login repositioning. Then reads where the
//! character stands from the client's own terrain.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use common::{ViewDistances, terrain::TerrainGrid, vol::ReadVol};
use tokio::runtime::Runtime;
use vek::{Vec2, Vec3};
use xindeler_client::Client;

use crate::{
    client_dump::{Driver, connect_bot},
    probe::{Probe, Res},
    scratch_server::{ScratchServer, ServerOpts},
};

pub struct LoginCheckOpts {
    pub at: Vec2<i32>,
    pub view_distance: u32,
    pub server_bin: PathBuf,
    pub server_ready_timeout: Duration,
    pub keep_server_log: Option<PathBuf>,
}

/// Same radius the server uses for the dry-ground search.
const DRY_RADIUS: i32 = 48;

fn block_desc(t: &TerrainGrid, p: Vec3<i32>) -> String {
    match t.get(p) {
        Ok(b) => format!(
            "{:?}{}",
            b.kind(),
            if b.is_liquid() { " (liquid)" } else { "" }
        ),
        Err(_) => "unloaded".to_owned(),
    }
}

fn tick_for(c: &mut Client, drv: &mut Driver, d: Duration) -> Res<()> {
    let t = Instant::now();
    while t.elapsed() < d {
        drv.tick(c)?;
    }
    Ok(())
}

/// Tick until the client holds the chunk under `wpos` (and its 8 neighbours).
fn wait_chunks(c: &mut Client, drv: &mut Driver, wpos: Vec2<i32>, limit: Duration) -> Res<()> {
    let t = Instant::now();
    let key = wpos.map(|e| e.div_euclid(32));
    loop {
        drv.tick(c)?;
        let terr = c.state().terrain();
        let all = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| key + Vec2::new(dx, dy)))
            .all(|k| terr.get_key(k).is_some());
        drop(terr);
        if all {
            return Ok(());
        }
        if t.elapsed() > limit {
            return Err(format!("chunks around {wpos} never arrived").into());
        }
    }
}

pub fn run(p: &Probe, o: &LoginCheckOpts) -> Res<bool> {
    let w = p
        .world
        .sim()
        .authored_water_at(o.at)
        .ok_or_else(|| format!("{} is not an authored wet column", o.at))?;
    eprintln!(
        "target {}: authored water surface block {}, bed block {} ({} blocks deep)",
        o.at,
        w.surface_block,
        w.bed_block,
        w.depth_blocks()
    );
    if w.depth_blocks() < 2 {
        return Err("pick a column at least 2 blocks deep".into());
    }

    let mut server = ScratchServer::start(&ServerOpts {
        server_bin: o.server_bin.clone(),
        assets: p.assets_root.clone(),
        seed: p.seed,
        view_distance: o.view_distance,
        ready_timeout: o.server_ready_timeout,
        keep_log_to: o.keep_server_log.clone(),
    })?;
    let runtime = Arc::new(Runtime::new()?);
    let mut drv = Driver::new();
    let mut client = connect_bot(&runtime, server.port, o.view_distance, &mut drv)?;

    // 1. Into the water, mid-depth, and save the waypoint there.
    let mid = (w.surface_block + w.bed_block + 1) / 2;
    client.send_command("goto".to_owned(), vec![
        o.at.x.to_string(),
        o.at.y.to_string(),
        mid.to_string(),
    ]);
    wait_chunks(&mut client, &mut drv, o.at, Duration::from_secs(180))?;
    tick_for(&mut client, &mut drv, Duration::from_secs(3))?;
    client.send_command("set_waypoint".to_owned(), vec![]);
    tick_for(&mut client, &mut drv, Duration::from_secs(2))?;
    let saved = client.position().ok_or("no position before logout")?;
    let saved_i = saved.map(|e| e.floor() as i32);
    let (plain, dry) = {
        let t = client.state().terrain();
        eprintln!(
            "saved position {saved:?}: block {} / below {}",
            block_desc(&t, saved_i),
            block_desc(&t, saved_i - Vec3::unit_z())
        );
        (
            t.try_find_ground(saved_i),
            t.try_find_dry_ground(saved_i, DRY_RADIUS),
        )
    };
    eprintln!("client-side search from the saved position: plain ground {plain:?}, dry {dry:?}");

    // 2. Back to the character list (saves the character), then select it again:
    //    the server loads it from its database and repositions it.
    let id = client
        .character_list()
        .characters
        .first()
        .and_then(|c| c.character.id)
        .ok_or("no character in the list")?;
    client.request_remove_character();
    let t = Instant::now();
    while client.presence().is_some() {
        drv.tick(&mut client)?;
        if t.elapsed() > Duration::from_secs(60) {
            return Err("never left the world".into());
        }
    }
    tick_for(&mut client, &mut drv, Duration::from_secs(3))?;
    client.request_character(id, ViewDistances {
        terrain: o.view_distance,
        entity: 4,
    });
    let t = Instant::now();
    while client.presence().is_none() || client.position().is_none() {
        drv.tick(&mut client)?;
        if t.elapsed() > Duration::from_secs(120) {
            return Err("never re-entered the world".into());
        }
    }
    eprintln!("re-entered at {:?}", client.position());
    wait_chunks(&mut client, &mut drv, o.at, Duration::from_secs(180))?;
    // Let repositioning and physics settle.
    tick_for(&mut client, &mut drv, Duration::from_secs(8))?;
    let fin = client.position().ok_or("no position after login")?;
    let fin_i = fin.map(|e| e.floor() as i32);
    let t = client.state().terrain();
    let at_feet = t.get(fin_i).map(|b| b.is_liquid()).unwrap_or(true);
    let below = t.get(fin_i - Vec3::unit_z()).ok().copied();
    let authored_wet = p.world.is_wet_at(fin_i.xy()).unwrap_or(false);
    eprintln!(
        "after login: position {fin:?} ({:.1} m from the saved one); block {} / below {}; \
         authored water at this column: {}",
        (fin.xy() - saved.xy()).magnitude(),
        block_desc(&t, fin_i),
        block_desc(&t, fin_i - Vec3::unit_z()),
        if authored_wet { "wet" } else { "dry" }
    );
    drop(t);
    if let Some(status) = server.exited()? {
        return Err(format!("server died ({status}):\n{}", server.log_tail(25)).into());
    }
    let ok = !at_feet && below.is_some_and(|b| b.is_solid() && !b.is_liquid());
    eprintln!(
        "{}",
        if ok {
            "PASS: the character logged in on dry ground"
        } else {
            "FAIL: the character logged in in water"
        }
    );
    Ok(ok)
}
