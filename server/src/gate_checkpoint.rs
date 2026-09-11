//! Server-owned reconciliation for Cromatolis's Green Post checkpoint gate:
//! a repeatable, bidirectional gate that opens while a nearby player holds a
//! valid, identity-bound permit item, and re-closes once no valid holder
//! remains nearby.
//!
//! Mirrors `citadel.rs`'s 1Hz `ensure_*` reconciliation pattern, but this
//! module writes terrain blocks (via [`common_state::State::set_block`])
//! instead of spawning/restoring entities, since the gate itself is baked
//! terrain geometry ([`world::site::plot::Fortification`]), not a physical
//! entity.
//!
//! This module also carries a second, unrelated reconciliation: a single
//! flavor-only stationed guard NPC near the Evercross settlement. It has no
//! interaction with the checkpoint mechanic above (Evercross anchors the
//! *north* gate, not Green Post) and lives here only because it is small
//! enough not to warrant its own module.

use crate::state_ext::StateExt;
use common::comp;
use common_state::State;
use specs::{Builder, Join, WorldExt};
use vek::{Vec2, Vec3};
use world::World;

/// The one real, authored gate this checkpoint mechanic governs. See
/// `assets/world/map/cromatolis_v0_fortifications.ron` -- the other two
/// real gates (`gate.northwall_black_iron`, permanently open, and
/// `gate.eastwall_black_iron`, permanently closed) are untouched by this
/// module.
const GREEN_POST_GATE_ID: &str = "gate.greenhwall_black_iron";

/// The settlement the flavor-only guard NPC (see the module doc) is
/// anchored to. Unrelated to [`GREEN_POST_GATE_ID`] above -- Evercross
/// anchors the north gate (`gate.northwall_black_iron`), not Green Post.
const EVERCROSS_SETTLEMENT_ID: &str = "site.evercross";

/// Asset id of the Olummiton Academy travel permit -- the item this
/// checkpoint checks for. Also consumed by the `/give_gate_permit` admin
/// command (`server/src/cmd.rs`), a stopgap grant path used until real
/// NPC/quest-dialogue content can grant it narratively.
pub const GREEN_POST_PERMIT_ITEM_ID: &str = "common.items.quest.olummiton_travel_permit";

/// A nearby player's held permit is only honored within this radius (world
/// blocks/metres) of the gate's own centre -- enough to feel like
/// "approaching a checkpoint," not the whole map.
const CHECKPOINT_RADIUS: f32 = 6.0;

/// A candidate entity counts as "the already-live Evercross guard" within
/// this radius of the settlement anchor.
const EVERCROSS_GUARD_RECOVERY_RADIUS: f32 = 6.0;

/// Loads a real chunk key at `pos`, mirroring `citadel.rs`'s own private
/// `terrain_home_chunk` helper -- kept as a separate small copy here rather
/// than shared, the same way `citadel.rs` doesn't expose its own version
/// either.
fn terrain_home_chunk(state: &State, pos: Vec3<f32>) -> Option<Vec2<i32>> {
    let terrain = state.terrain();
    let chunk = terrain.pos_key(pos.map(|axis| axis.floor() as i32));
    terrain.get_key_real(chunk).is_some().then_some(chunk)
}

fn gate_center(gate_aabb: vek::Aabb<i32>) -> Vec3<f32> {
    (gate_aabb.min.as_::<f32>() + gate_aabb.max.as_::<f32>()) / 2.0
}

/// Whether any player within [`CHECKPOINT_RADIUS`] of `center` is carrying
/// the Green Post permit ([`GREEN_POST_PERMIT_ITEM_ID`]) bound
/// ([`comp::Item::owner`]) to their own `CharacterId`.
///
/// [`comp::Item::owner`] is generic per-instance identity-binding infra, not
/// Green-Post-specific -- checking it alone (without also checking the item
/// definition id) would open this gate for *any* item some other feature
/// happens to bind to the nearby player, not just the permit.
fn has_valid_holder_nearby(state: &State, center: Vec3<f32>) -> bool {
    let ecs = state.ecs();
    let entities = ecs.entities();
    let positions = ecs.read_storage::<comp::Pos>();
    let presences = ecs.read_storage::<comp::Presence>();
    let inventories = ecs.read_storage::<comp::Inventory>();

    (&entities, &positions, &presences, &inventories)
        .join()
        .any(|(_entity, pos, presence, inventory)| {
            let Some(character_id) = presence.kind.character_id() else {
                return false;
            };
            if (pos.0 - center).magnitude_squared() > CHECKPOINT_RADIUS.powi(2) {
                return false;
            }
            inventory.slots().flatten().any(|item| {
                item.owner() == Some(character_id)
                    && item.item_definition_id().itemdef_id() == Some(GREEN_POST_PERMIT_ITEM_ID)
            })
        })
}

/// The block a fully open/closed gate should show at every position within
/// its AABB.
fn gate_state_block(open: bool) -> common::terrain::Block {
    if open {
        common::terrain::Block::empty()
    } else {
        world::site::plot::Fortification::gate_closed_block()
    }
}

/// Writes every block in `gate_aabb` to `open`'s desired state -- cleared if
/// `open`, filled with the fortification's own closed-gate material
/// otherwise.
fn write_gate_state(state: &State, gate_aabb: vek::Aabb<i32>, open: bool) {
    let block = gate_state_block(open);
    for x in gate_aabb.min.x..gate_aabb.max.x {
        for y in gate_aabb.min.y..gate_aabb.max.y {
            for z in gate_aabb.min.z..gate_aabb.max.z {
                state.set_block(Vec3::new(x, y, z), block);
            }
        }
    }
}

/// Whether the gate's *live* terrain no longer matches `desired_open` --
/// e.g. a player mined through a closed gate, or built a wall into an open
/// one. Checked every pass (in addition to the `last_known_open` cache) so
/// the reconciliation is genuinely self-healing against player-caused
/// terrain drift, not only against a server restart -- unlike a plain cache
/// comparison, this reads back the actual `TerrainGrid` state, the same
/// live-state-diffing property `citadel::component_needs_restore` provides
/// for entity components.
///
/// The gate's AABB is small (a handful of blocks in each dimension), so
/// scanning it in full each pass is cheap -- much cheaper than the 1Hz
/// player-proximity scan this module already runs alongside.
fn gate_state_has_drifted(state: &State, gate_aabb: vek::Aabb<i32>, desired_open: bool) -> bool {
    let expected = gate_state_block(desired_open);
    for x in gate_aabb.min.x..gate_aabb.max.x {
        for y in gate_aabb.min.y..gate_aabb.max.y {
            for z in gate_aabb.min.z..gate_aabb.max.z {
                // `None` means the position isn't loaded/known yet -- not a
                // drift, just missing data; skip rather than forcing a
                // write against terrain that isn't live.
                if let Some(block) = state.get_block(Vec3::new(x, y, z))
                    && block != expected
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Reconciles the Green Post checkpoint gate against nearby players' held
/// permits.
///
/// `last_known_open` is the caller's own cached last-applied state (a plain
/// field on `Server`, mirroring `citadel_defence_refresh`'s own
/// plain-`Duration`-field-on-`Server` shape rather than an ECS resource) --
/// a fast-path skip for the common case where nothing changed. It is never
/// trusted on its own: [`gate_state_has_drifted`] re-reads the actual live
/// terrain every pass, so a player who mines/builds through the gate gets
/// it corrected on the next pass even though `last_known_open` never
/// toggled. `None` (server start, or after a hot-reload) always forces one
/// write, self-healing the gate into a known state the same way
/// `citadel::ensure_upper_defences` recovers its own stations after a
/// restart.
///
/// Returns whether the gate's physical state changed this call.
pub fn ensure_green_post_checkpoint(
    state: &mut State,
    world: &World,
    last_known_open: &mut Option<bool>,
) -> bool {
    let Some(gate_aabb) =
        world::civ::cromatolis_fortification_gate_world_aabb(world.sim(), GREEN_POST_GATE_ID)
    else {
        return false;
    };
    let center = gate_center(gate_aabb);

    // Defer to a later pass rather than queuing a write against terrain that
    // isn't live yet -- mirrors `citadel::ensure_upper_defences`'s own
    // `terrain_home_chunk` gate.
    if terrain_home_chunk(state, center).is_none() {
        return false;
    }

    let desired_open = has_valid_holder_nearby(state, center);
    let up_to_date = *last_known_open == Some(desired_open)
        && !gate_state_has_drifted(state, gate_aabb, desired_open);
    if up_to_date {
        return false;
    }

    write_gate_state(state, gate_aabb, desired_open);
    *last_known_open = Some(desired_open);
    true
}

/// Builds and spawns the stationary flavor guard, mirroring the
/// `LoadoutBuilder::from_asset_expect` + `State::create_npc` shape
/// `/spawn`'s handler (`server/src/cmd.rs`) already uses for a generic
/// archetype-driven NPC.
fn spawn_evercross_guard(state: &mut State, pos: Vec3<f32>, home_chunk: Vec2<i32>) {
    let mut rng = rand::rng();
    let body = comp::Body::Humanoid(comp::body::humanoid::Body::random());
    let loadout = comp::inventory::loadout_builder::LoadoutBuilder::from_asset_expect(
        "common.loadout.village.guard",
        &mut rng,
        None,
    )
    .build();
    let inventory = comp::Inventory::with_loadout(loadout, body);
    let stats = comp::Stats::new(comp::Content::Plain("Evercross Guard".to_string()), body);
    let agent = comp::Agent::from_body(&body).with_patrol_origin(pos);

    state
        .create_npc(
            comp::Pos(pos),
            comp::Ori::default(),
            stats,
            comp::SkillSet::default(),
            Some(comp::Health::new(body)),
            comp::Poise::new(body),
            inventory,
            body,
            body.scale(),
        )
        .with(agent)
        .with(comp::Anchor::Chunk(home_chunk))
        .build();
}

/// Ensures a single, stationary guard NPC exists near the Evercross
/// settlement anchor -- flavor only (see the module doc), no interaction
/// with the checkpoint mechanic above. Mirrors
/// `citadel::ensure_upper_defences`'s "only materializes an absent station"
/// shape, but for a humanoid NPC built from an existing generic guard
/// archetype (`common.entity.village.guard`'s loadout) rather than a
/// bespoke one.
///
/// Returns whether a guard was spawned this call.
pub fn ensure_evercross_guard(state: &mut State, world: &World) -> bool {
    let Some(anchor) =
        world::civ::cromatolis_settlement_world_center(world.sim(), EVERCROSS_SETTLEMENT_ID)
    else {
        return false;
    };
    let land = world::Land::from_sim(world.sim());
    let pos = Vec3::new(
        anchor.x as f32,
        anchor.y as f32,
        land.get_alt_approx(anchor) + 1.0,
    );

    let Some(home_chunk) = terrain_home_chunk(state, pos) else {
        return false;
    };

    let already_present = {
        let ecs = state.ecs();
        let entities = ecs.entities();
        let bodies = ecs.read_storage::<comp::Body>();
        let positions = ecs.read_storage::<comp::Pos>();
        let alignments = ecs.read_storage::<comp::Alignment>();
        (&entities, &bodies, &positions, &alignments).join().any(
            |(_, body, entity_pos, alignment)| {
                body.is_humanoid()
                    && matches!(alignment, comp::Alignment::Npc)
                    && (entity_pos.0.xy() - pos.xy()).magnitude_squared()
                        <= EVERCROSS_GUARD_RECOVERY_RADIUS.powi(2)
            },
        )
    };
    if already_present {
        return false;
    }

    spawn_evercross_guard(state, pos, home_chunk);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        character::CharacterId,
        comp::{Inventory, Item, Presence},
        resources::GameMode,
        terrain::{MapSizeLg, TerrainChunk, TerrainGrid},
    };
    use specs::WorldExt;
    use std::sync::Arc;
    use vek::Aabb;

    const WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    fn setup() -> State {
        let pools = State::pools(GameMode::Server);
        let mut state = State::new(
            GameMode::Server,
            pools,
            WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                common_systems::add_local_systems(dispatch_builder);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        // `Presence` and `Anchor` are server-only components, normally
        // registered by `Server::new` -- the checkpoint/guard reconciliation
        // tests below need them registered by hand here, the same way
        // `citadel.rs`'s own idempotency tests register `comp::Anchor`.
        state.ecs_mut().register::<Presence>();
        state.ecs_mut().register::<comp::Anchor>();
        // `State::apply_terrain_changes` (used by the block-write test below
        // to observe a queued `BlockChange`) reads this resource to report
        // sprite-removal outcomes -- normally inserted by `Server::new`.
        state
            .ecs_mut()
            .insert(common::event::EventBus::<common::event::BonkEvent>::default());
        state
    }

    fn load_chunk_containing(state: &mut State, pos: Vec3<f32>) {
        let key = state.terrain().pos_key(pos.map(|axis| axis.floor() as i32));
        state
            .ecs_mut()
            .write_resource::<TerrainGrid>()
            .insert(key, Arc::new(TerrainChunk::water(0)));
    }

    fn spawn_player(
        state: &mut State,
        pos: Vec3<f32>,
        character_id: CharacterId,
        item: Option<Item>,
    ) {
        use common::{
            ViewDistances,
            comp::presence::{Presence as PresenceComp, PresenceKind},
        };

        let mut inventory = Inventory::with_empty();
        if let Some(item) = item {
            inventory.push(item).expect("empty inventory has room");
        }

        state
            .ecs_mut()
            .create_entity()
            .with(comp::Pos(pos))
            .with(inventory)
            .with(PresenceComp::new(
                ViewDistances {
                    terrain: 1,
                    entity: 1,
                },
                PresenceKind::Character(character_id),
            ))
            .build();
    }

    fn owned_permit(owner: CharacterId) -> Item {
        let mut item = Item::new_from_asset_expect(GREEN_POST_PERMIT_ITEM_ID);
        item.set_owner(owner);
        item
    }

    #[test]
    fn write_gate_state_open_clears_and_closed_fills_the_aabb() {
        let mut state = setup();
        let aabb = Aabb {
            min: Vec3::new(0, 0, 0),
            max: Vec3::new(2, 2, 2),
        };
        load_chunk_containing(&mut state, Vec3::new(1.0, 1.0, 1.0));

        write_gate_state(&state, aabb, false);
        state.apply_terrain_changes(|_, _| {});
        assert_eq!(
            state.get_block(Vec3::new(1, 1, 1)),
            Some(world::site::plot::Fortification::gate_closed_block()),
            "a closed gate must fill solid with the fortification's own material"
        );

        write_gate_state(&state, aabb, true);
        state.apply_terrain_changes(|_, _| {});
        assert_eq!(
            state.get_block(Vec3::new(1, 1, 1)),
            Some(common::terrain::Block::empty()),
            "an open gate must clear a passable gap"
        );
    }

    #[test]
    fn gate_state_has_drifted_detects_terrain_mined_through_a_closed_gate() {
        let mut state = setup();
        let aabb = Aabb {
            min: Vec3::new(0, 0, 0),
            max: Vec3::new(2, 2, 2),
        };
        load_chunk_containing(&mut state, Vec3::new(1.0, 1.0, 1.0));

        write_gate_state(&state, aabb, false);
        state.apply_terrain_changes(|_, _| {});
        assert!(
            !gate_state_has_drifted(&state, aabb, false),
            "a freshly-written closed gate must not report drift against itself"
        );

        // Simulate a player mining a single block out of the closed gate,
        // without going through `ensure_green_post_checkpoint` at all.
        state.set_block(Vec3::new(1, 1, 1), common::terrain::Block::empty());
        state.apply_terrain_changes(|_, _| {});
        assert!(
            gate_state_has_drifted(&state, aabb, false),
            "a block mined out of a closed gate must be detected as drift"
        );

        write_gate_state(&state, aabb, false);
        state.apply_terrain_changes(|_, _| {});
        assert!(
            !gate_state_has_drifted(&state, aabb, false),
            "rewriting the gate state must clear the detected drift"
        );
    }

    #[test]
    fn only_the_permit_item_counts_not_any_item_bound_to_the_player() {
        let character_id = CharacterId(4);
        let center = Vec3::new(20.0, 20.0, 20.0);

        let mut state = setup();
        let mut unrelated_item = Item::new_from_asset_expect("common.items.weapons.empty.empty");
        unrelated_item.set_owner(character_id);
        spawn_player(&mut state, center, character_id, Some(unrelated_item));

        assert!(
            !has_valid_holder_nearby(&state, center),
            "an item bound to the player's own character that isn't the permit must not open the \
             gate"
        );
    }

    #[test]
    fn holder_must_own_the_item_themselves() {
        let character_id = CharacterId(1);
        let other_character_id = CharacterId(2);
        let center = Vec3::new(10.0, 10.0, 10.0);

        let mut state = setup();
        spawn_player(
            &mut state,
            center,
            character_id,
            Some(owned_permit(other_character_id)),
        );
        assert!(
            !has_valid_holder_nearby(&state, center),
            "an item bound to a different character must not open the gate"
        );

        let mut state = setup();
        spawn_player(&mut state, center, character_id, None);
        assert!(
            !has_valid_holder_nearby(&state, center),
            "no item at all must not open the gate"
        );

        let mut state = setup();
        spawn_player(
            &mut state,
            center,
            character_id,
            Some(owned_permit(character_id)),
        );
        assert!(
            has_valid_holder_nearby(&state, center),
            "an item bound to this player's own character must open the gate"
        );
    }

    #[test]
    fn holder_outside_the_checkpoint_radius_does_not_count() {
        let character_id = CharacterId(3);
        let center = Vec3::new(0.0, 0.0, 0.0);
        let far_away = center + Vec3::new(CHECKPOINT_RADIUS * 4.0, 0.0, 0.0);

        let mut state = setup();
        spawn_player(
            &mut state,
            far_away,
            character_id,
            Some(owned_permit(character_id)),
        );
        assert!(
            !has_valid_holder_nearby(&state, center),
            "a valid holder far outside the checkpoint radius must not open the gate"
        );
    }

    // ---- Heavy, real-terrain-backed test: requires the real Cromatolis LFS
    // assets pulled locally. Not run automatically (same precedent as
    // `world::civ::tests`'s own `..._against_real_lfs_assets` tests).
    // Recommended: `cargo test -p xindeler-server -- --ignored green_post` ----

    /// `ensure_green_post_checkpoint` must not re-queue block writes on a
    /// second pass when nothing about the nearby holders changed -- the
    /// property the whole 1Hz refresh depends on, exercised here against a
    /// real generated Cromatolis `World`/`WorldSim`, mirroring
    /// `citadel.rs`'s own `idempotency_tests` module. It must also
    /// self-heal a closed gate that a player mined through even though
    /// `last_known_open` never toggled -- the property `gate_state_has_drifted`
    /// exists to guarantee.
    #[test]
    #[ignore]
    fn ensure_green_post_checkpoint_is_idempotent_and_self_heals_against_the_real_world() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, _index) = world::World::generate(
            0,
            world::sim::WorldOpts {
                seed_elements: true,
                world_file: world::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );

        let gate_aabb =
            world::civ::cromatolis_fortification_gate_world_aabb(world.sim(), GREEN_POST_GATE_ID)
                .expect("the real export must carry the Green Post gate");
        let center = gate_center(gate_aabb);

        let mut state = setup();
        load_chunk_containing(&mut state, center);
        spawn_player(
            &mut state,
            center,
            CharacterId(1),
            Some(owned_permit(CharacterId(1))),
        );

        let mut last_known_open = None;
        assert!(
            ensure_green_post_checkpoint(&mut state, &world, &mut last_known_open),
            "the first pass must open the gate for the nearby valid holder"
        );
        assert_eq!(last_known_open, Some(true));

        assert!(
            !ensure_green_post_checkpoint(&mut state, &world, &mut last_known_open),
            "a second pass with unchanged conditions must not redundantly write blocks"
        );

        // Move the holder far away so the gate is due to close, then let it
        // actually close.
        {
            let mut positions = state.ecs_mut().write_storage::<comp::Pos>();
            for pos in (&mut positions).join() {
                pos.0 = center + Vec3::new(CHECKPOINT_RADIUS * 10.0, 0.0, 0.0);
            }
        }
        assert!(
            ensure_green_post_checkpoint(&mut state, &world, &mut last_known_open),
            "the gate must close once the holder leaves the checkpoint radius"
        );
        assert_eq!(last_known_open, Some(false));
        // Actually apply the queued close write so the block read-back
        // below reflects it, matching how the server applies every tick's
        // `BlockChange` before the next 1Hz reconciliation pass runs.
        state.apply_terrain_changes(|_, _| {});

        // Simulate a player mining a single block out of the now-closed
        // gate -- entirely outside `ensure_green_post_checkpoint`, the same
        // way a real player's terrain edit would happen -- and apply it.
        let sample = gate_center(gate_aabb).map(|c| c.floor() as i32);
        state.set_block(sample, common::terrain::Block::empty());
        state.apply_terrain_changes(|_, _| {});

        assert!(
            ensure_green_post_checkpoint(&mut state, &world, &mut last_known_open),
            "a pass must detect and repair terrain drift even though last_known_open never toggled"
        );
        state.apply_terrain_changes(|_, _| {});
        assert_eq!(
            state.get_block(sample),
            Some(world::site::plot::Fortification::gate_closed_block()),
            "the mined-out block must be refilled"
        );
    }

    /// `ensure_evercross_guard` must spawn exactly one guard near the real
    /// Evercross anchor, then never spawn a second one on a later pass --
    /// exercised against a real generated Cromatolis `World`, same rationale
    /// as the checkpoint test above.
    #[test]
    #[ignore]
    fn ensure_evercross_guard_spawns_once_against_the_real_world() {
        let threadpool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let (world, _index) = world::World::generate(
            0,
            world::sim::WorldOpts {
                seed_elements: true,
                world_file: world::sim::FileOpts::LoadAsset("world.map.cromatolis_v0".to_string()),
                calendar: None,
            },
            &threadpool,
            &|_| {},
        );

        let anchor =
            world::civ::cromatolis_settlement_world_center(world.sim(), EVERCROSS_SETTLEMENT_ID)
                .expect("the real export must carry the Evercross settlement");
        let land = world::Land::from_sim(world.sim());
        let pos = Vec3::new(
            anchor.x as f32,
            anchor.y as f32,
            land.get_alt_approx(anchor) + 1.0,
        );

        let mut state = setup();
        load_chunk_containing(&mut state, pos);

        assert!(
            ensure_evercross_guard(&mut state, &world),
            "the first pass must spawn the Evercross guard"
        );
        assert!(
            !ensure_evercross_guard(&mut state, &world),
            "a second pass must recognize the already-live guard and not spawn a duplicate"
        );
    }
}
