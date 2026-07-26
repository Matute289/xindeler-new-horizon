//! Integration tests for `CharacterState::TelekineticGrip` — the shared
//! grab/hold/release-item mechanism. Exercises the real ECS dispatch
//! (`character_behavior` + `phys` + `projectile`, plus the private `tether`
//! system pulled in via `add_local_systems`) rather than calling `behavior()`
//! directly, so the tests double as end-to-end coverage of the `Tethered`
//! link reuse.

#[cfg(test)]
mod tests {
    use common::{
        DamageKind,
        comp::{
            Body, CapsulePrism, CharacterActivity, CharacterState, Collider, Controller,
            ControllerInputs, Energy, Health, Immovable, InputAttr, InputKind, Ori, PhysicsState,
            PickupItem, Poise, Pos, Stats, Vel,
            inventory::item::{Item, MaterialStatManifest},
            item::tool::AbilityMap,
            projectile::{ProjectileAttack, ProjectileConstructor, ProjectileConstructorKind},
        },
        event::{EventBus, HealthChangeEvent, KnockbackEvent},
        link::{Is, LinkHandle},
        resources::{DeltaTime, GameMode, ProgramTime, Secs, Time},
        shared_server_config::ServerConstants,
        skillset_builder::SkillSetBuilder,
        states::{
            telekinetic_grip::{self, StaticData},
            utils::{AbilityInfo, StageSection},
        },
        terrain::{
            Block, BlockKind, MapSizeLg, SpriteKind, TerrainChunk, TerrainChunkMeta, TerrainGrid,
        },
        tether::{Follower, Leader, Tethered},
        uid::Uid,
        util::Dir,
    };
    use common_net::sync::WorldSyncExt;
    use common_state::State;
    use specs::{Builder, Entity, WorldExt};
    use std::{sync::Arc, time::Duration};
    use vek::{Rgb, Vec2, Vec3};
    use xindeler_common_systems::add_local_systems;

    const DEFAULT_WORLD_CHUNKS_LG: MapSizeLg =
        if let Ok(map_size_lg) = MapSizeLg::new(Vec2 { x: 10, y: 10 }) {
            map_size_lg
        } else {
            panic!("Default world chunk size does not satisfy required invariants.");
        };

    /// Deterministic chunk of solid ground, tall enough that none of this
    /// file's test positions (all near the origin, at z=0) fall underneath
    /// it — mirrors `common/systems/tests/phys/utils.rs`'s fixture, needed so
    /// `phys::Sys` treats these positions as "in a loaded chunk" and actually
    /// integrates velocity into position (it no-ops entities outside loaded
    /// terrain).
    fn generate_chunk(state: &mut State, chunk_pos: Vec2<i32>) {
        state.ecs().write_resource::<TerrainGrid>().insert(
            chunk_pos,
            Arc::new(TerrainChunk::new(
                // Well below z=0 so every entity this file creates (all near the
                // origin, at z=0) floats in open air above solid ground rather than
                // sitting exactly on/in the surface.
                -50,
                Block::new(BlockKind::Grass, Rgb::new(11, 102, 35)),
                Block::air(SpriteKind::Empty),
                TerrainChunkMeta::void(),
            )),
        );
    }

    fn setup() -> State {
        let pools = State::pools(GameMode::Server);
        let mut state = State::new(
            GameMode::Server,
            pools,
            DEFAULT_WORLD_CHUNKS_LG,
            Arc::new(TerrainChunk::water(0)),
            |dispatch_builder| {
                add_local_systems(dispatch_builder);
            },
            #[cfg(feature = "plugins")]
            common_state::plugin::PluginMgr::default(),
        );
        let msm = MaterialStatManifest::load().cloned();
        state.ecs_mut().insert(msm);
        state.ecs_mut().insert(AbilityMap::load().cloned());
        state.ecs_mut().read_resource::<Time>();
        state.ecs_mut().read_resource::<DeltaTime>();
        // `event_emitters!` (see `common/src/event.rs`) fetches these as
        // `Option<Read<EventBus<_>>>`, so combat code silently no-ops instead of
        // panicking when they're missing — normally fine, but it means a test
        // that wants to observe combat events (unlike production, where the
        // server explicitly registers every event bus — see
        // `server/src/events/event_types.rs::register_event_busses`) has to
        // insert them itself.
        state
            .ecs_mut()
            .insert(EventBus::<HealthChangeEvent>::default());
        state
            .ecs_mut()
            .insert(EventBus::<KnockbackEvent>::default());
        for x in 0..2 {
            for y in 0..2 {
                generate_chunk(&mut state, Vec2::new(x, y));
            }
        }
        state
    }

    fn tick(state: &mut State, dt: Duration) {
        state.tick(
            dt,
            false,
            None,
            &ServerConstants {
                day_cycle_coefficient: 24.0,
                oracle_live: false,
            },
            |_, _| {},
        );
    }

    fn uid_of(state: &State, entity: Entity) -> Uid {
        *state
            .ecs()
            .read_storage::<Uid>()
            .get(entity)
            .expect("entity created via create_entity_synced should have a Uid")
    }

    /// Registers a bare entity into `IdMaps` (so `id_maps.uid_entity` can
    /// resolve it) without attaching any other components — used for the
    /// "leader" side of an "already gripped by someone else" fixture, where
    /// only a valid, resolvable `Uid` is needed.
    fn create_bare_synced_entity(state: &mut State) -> (Entity, Uid) {
        let entity = state.ecs_mut().create_entity_synced().build();
        let uid = uid_of(state, entity);
        (entity, uid)
    }

    fn create_caster(state: &mut State, ability: StaticData) -> Entity {
        let body = common::comp::Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut rand::rng(),
            &common::comp::humanoid::Species::Human,
        ));
        let skill_set = SkillSetBuilder::default().build();
        state
            .ecs_mut()
            .create_entity_synced()
            .with(CharacterState::TelekineticGrip(telekinetic_grip::Data {
                static_data: ability,
                timer: Duration::default(),
                stage_section: StageSection::Buildup,
                item: None,
                thrown: false,
            }))
            .with(CharacterActivity::default())
            .with(Pos(Vec3::zero()))
            .with(Vel::default())
            .with(Ori::default())
            .with(body.mass())
            .with(body.density())
            .with(body)
            .with(Energy::new(body))
            .with(Controller {
                inputs: ControllerInputs {
                    look_dir: Dir::default(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .with(Poise::new(body))
            .with(skill_set)
            .with(PhysicsState::default())
            .with(Stats::empty(body))
            .build()
    }

    fn create_item(state: &mut State, pos: Vec3<f32>) -> (Entity, Uid) {
        let item = Item::new_from_asset_expect("common.items.testing.test_boots");
        let item_body = common::comp::body::item::Body::from(&item);
        let body = Body::Item(item_body);
        let pickup = PickupItem::new(item, ProgramTime(0.0), false);
        let entity = state
            .ecs_mut()
            .create_entity_synced()
            .with(pickup)
            .with(Pos(pos))
            .with(Ori::default())
            .with(Vel::default())
            .with(body.mass())
            .with(body.density())
            .with(body.collider())
            .with(body)
            .build();
        let uid = uid_of(state, entity);
        (entity, uid)
    }

    fn create_creature(state: &mut State, pos: Vec3<f32>) -> (Entity, Uid) {
        let body = common::comp::Body::Humanoid(common::comp::humanoid::Body::random_with(
            &mut rand::rng(),
            &common::comp::humanoid::Species::Human,
        ));
        let (p0, p1, radius) = body.sausage();
        let collider = Collider::CapsulePrism(CapsulePrism {
            p0,
            p1,
            radius,
            z_min: 0.0,
            z_max: body.height(),
        });
        let entity = state
            .ecs_mut()
            .create_entity_synced()
            .with(Pos(pos))
            .with(Ori::default())
            .with(Vel::default())
            .with(body.mass())
            .with(body.density())
            .with(collider)
            .with(body)
            .with(Health::new(body))
            .with(Stats::empty(body))
            .with(PhysicsState::default())
            .build();
        let uid = uid_of(state, entity);
        (entity, uid)
    }

    fn default_projectile() -> ProjectileConstructor {
        ProjectileConstructor {
            kind: ProjectileConstructorKind::Simple,
            attack: Some(ProjectileAttack {
                damage: 20.0,
                poise: None,
                knockback: Some(8.0),
                energy: None,
                buff: None,
                friendly_fire: false,
                blockable: false,
                damage_effect: None,
                attack_effect: None,
                without_combo: true,
                damage_kind: DamageKind::Crushing,
            }),
            scaled: None,
            homing_rate: None,
            split: None,
            lifetime_override: Some(Secs(5.0)),
            limit_per_ability: false,
            override_collider: None,
            pierce_entities: true,
            is_point: false,
            is_sticky: false,
            hazard: false,
        }
    }

    fn default_ability(target: Option<Uid>) -> StaticData {
        StaticData {
            buildup_duration: Duration::from_millis(10),
            charge_duration: Duration::from_millis(200),
            place_threshold: Duration::from_millis(50),
            recover_duration: Duration::from_millis(10),
            energy_drain: 10.0,
            range: 10.0,
            // Generously larger than the item's starting distance (plus any
            // body-specific tether attach/hold offsets) so the tether pull
            // system never applies a pull impulse during these tests — keeps
            // the velocity assertions below deterministic.
            tether_length: 5.0,
            initial_projectile_speed: 5.0,
            scaled_projectile_speed: 15.0,
            projectile: default_projectile(),
            move_speed: 1.0,
            ability_info: AbilityInfo {
                tool: None,
                hand: None,
                input: InputKind::Ability(0),
                input_attr: Some(InputAttr {
                    select_pos: None,
                    target_entity: target,
                }),
                ability_meta: Default::default(),
                ability: None,
            },
        }
    }

    /// Advances past the ability's buildup stage so the next tick's
    /// `behavior()` call attempts the grab.
    ///
    /// Two things need small steps rather than one big tick: (1) the
    /// buildup->grab transition checks `self.timer < buildup_duration`
    /// against the *pre-tick* timer, so the tick that finally observes
    /// `timer >= buildup_duration` has to be a tick *after* the one that
    /// pushed it over the line; (2) `PreviousPhysCache` (used to read a
    /// target's position for the range check — see
    /// `telekinetic_grip::Data::try_grab`) is only populated once `phys::Sys`
    /// has run at least once, so it needs to be warm before that transition
    /// tick too.
    fn complete_buildup(state: &mut State, ability: &StaticData) {
        let step = Duration::from_millis(1);
        let steps = (ability.buildup_duration.as_millis() as u64) + 3;
        for _ in 0..steps {
            tick(state, step);
        }
    }

    fn set_held(state: &mut State, entity: Entity, ability: &StaticData, held: bool) {
        let mut controllers = state.ecs().write_storage::<Controller>();
        let controller = controllers.get_mut(entity).unwrap();
        if held {
            controller
                .queued_inputs
                .insert(ability.ability_info.input, InputAttr {
                    select_pos: None,
                    target_entity: None,
                });
        } else {
            controller.queued_inputs.remove(&ability.ability_info.input);
        }
    }

    /// (1) Buildup-stage validation rejects a target that isn't a
    /// `PickupItem`, is `Immovable`, is out of range, or is already gripped —
    /// and accepts a valid one.
    #[test]
    fn grip_validation() {
        // Not a PickupItem at all (a bare entity with just a Pos).
        {
            let mut state = setup();
            let (entity, uid) = create_bare_synced_entity(&mut state);
            state
                .ecs_mut()
                .write_storage::<Pos>()
                .insert(entity, Pos(Vec3::new(1.0, 0.0, 0.0)))
                .unwrap();
            let ability = default_ability(Some(uid));
            let caster = create_caster(&mut state, ability.clone());
            set_held(&mut state, caster, &ability, true);
            complete_buildup(&mut state, &ability);

            let is_followers = state.ecs().read_storage::<Is<Follower>>();
            assert!(
                is_followers.get(entity).is_none(),
                "non-PickupItem target must not become gripped"
            );
        }

        // Immovable.
        {
            let mut state = setup();
            let (item_entity, item_uid) = create_item(&mut state, Vec3::new(1.0, 0.0, 0.0));
            state
                .ecs_mut()
                .write_storage::<Immovable>()
                .insert(item_entity, Immovable)
                .unwrap();
            let ability = default_ability(Some(item_uid));
            let caster = create_caster(&mut state, ability.clone());
            set_held(&mut state, caster, &ability, true);
            complete_buildup(&mut state, &ability);

            let is_followers = state.ecs().read_storage::<Is<Follower>>();
            assert!(
                is_followers.get(item_entity).is_none(),
                "an Immovable target must not become gripped"
            );
        }

        // Out of range.
        {
            let mut state = setup();
            let (item_entity, item_uid) = create_item(&mut state, Vec3::new(1000.0, 0.0, 0.0));
            let ability = default_ability(Some(item_uid));
            let caster = create_caster(&mut state, ability.clone());
            set_held(&mut state, caster, &ability, true);
            complete_buildup(&mut state, &ability);

            let is_followers = state.ecs().read_storage::<Is<Follower>>();
            assert!(
                is_followers.get(item_entity).is_none(),
                "an out-of-range target must not become gripped"
            );
        }

        // Already gripped by someone else.
        {
            let mut state = setup();
            let (item_entity, item_uid) = create_item(&mut state, Vec3::new(1.0, 0.0, 0.0));
            let (other_leader_entity, other_leader_uid) = create_bare_synced_entity(&mut state);
            {
                // Both halves of the link need to exist for it to read as a live
                // tether rather than a stale one — `tether::Sys` self-heals (drops)
                // a follower whose leader doesn't carry `Is<Leader>` (see
                // `common/systems/src/tether.rs`), which would otherwise make this
                // fixture disappear before the caster's own grab attempt even runs.
                let handle = LinkHandle::from_link(Tethered {
                    leader: other_leader_uid,
                    follower: item_uid,
                    tether_length: 1.0,
                });
                state
                    .ecs_mut()
                    .write_storage::<Is<Leader>>()
                    .insert(other_leader_entity, handle.make_role::<Leader>())
                    .unwrap();
                state
                    .ecs_mut()
                    .write_storage::<Is<Follower>>()
                    .insert(item_entity, handle.make_role::<Follower>())
                    .unwrap();
            }
            let ability = default_ability(Some(item_uid));
            let caster = create_caster(&mut state, ability.clone());
            set_held(&mut state, caster, &ability, true);
            complete_buildup(&mut state, &ability);

            let is_leaders = state.ecs().read_storage::<Is<Leader>>();
            assert!(
                is_leaders.get(caster).is_none(),
                "grabbing an already-gripped item must not make the caster a leader"
            );
        }

        // Valid target: succeeds.
        {
            let mut state = setup();
            let (item_entity, item_uid) = create_item(&mut state, Vec3::new(1.0, 0.0, 0.0));
            let ability = default_ability(Some(item_uid));
            let caster = create_caster(&mut state, ability.clone());
            set_held(&mut state, caster, &ability, true);
            complete_buildup(&mut state, &ability);

            let is_followers = state.ecs().read_storage::<Is<Follower>>();
            assert!(
                is_followers.get(item_entity).is_some(),
                "a valid, in-range, ungripped PickupItem must become gripped"
            );
            let is_leaders = state.ecs().read_storage::<Is<Leader>>();
            assert!(is_leaders.get(caster).is_some());
            let char_states = state.ecs().read_storage::<CharacterState>();
            match char_states.get(caster) {
                Some(CharacterState::TelekineticGrip(data)) => {
                    assert_eq!(data.stage_section, StageSection::Charge);
                    assert_eq!(data.item, Some(item_uid));
                },
                other => panic!("expected TelekineticGrip/Charge, got {other:?}"),
            }
        }
    }

    /// (2) Grip-then-release-as-place leaves the item at rest near the
    /// caster with no velocity.
    #[test]
    fn release_as_place() {
        let mut state = setup();
        let (item_entity, item_uid) = create_item(&mut state, Vec3::new(1.0, 0.0, 0.0));
        let ability = default_ability(Some(item_uid));
        let caster = create_caster(&mut state, ability.clone());
        set_held(&mut state, caster, &ability, true);

        // Complete buildup -> grab succeeds, enters Charge.
        complete_buildup(&mut state, &ability);
        {
            let char_states = state.ecs().read_storage::<CharacterState>();
            assert!(matches!(
                char_states.get(caster),
                Some(CharacterState::TelekineticGrip(d)) if d.stage_section == StageSection::Charge
            ));
        }

        // Release immediately (well under `place_threshold`).
        set_held(&mut state, caster, &ability, false);
        tick(&mut state, Duration::from_millis(1));

        let is_followers = state.ecs().read_storage::<Is<Follower>>();
        assert!(
            is_followers.get(item_entity).is_none(),
            "the item must be released (untethered) immediately on place"
        );
        drop(is_followers);
        let velocities = state.ecs().read_storage::<Vel>();
        let vel = velocities.get(item_entity).expect("item lost its Vel");
        assert!(
            vel.0.magnitude_squared() < 1e-6,
            "a placed item should be at rest, got {vel:?}"
        );
    }

    /// (3) Grip-then-release-as-throw applies the expected `Vel` magnitude
    /// per the charge-based speed formula.
    #[test]
    fn release_as_throw() {
        let mut state = setup();
        let (item_entity, item_uid) = create_item(&mut state, Vec3::new(1.0, 0.0, 0.0));
        let ability = default_ability(Some(item_uid));
        let caster = create_caster(&mut state, ability.clone());
        set_held(&mut state, caster, &ability, true);

        complete_buildup(&mut state, &ability);

        // Hold well past `place_threshold` (and up to `charge_duration`) before
        // releasing, so this reads as a deliberate throw at (close to) full charge.
        let hold_ticks: u32 = 30;
        let dt = ability.charge_duration / hold_ticks;
        for _ in 0..hold_ticks {
            tick(&mut state, dt);
        }

        set_held(&mut state, caster, &ability, false);
        tick(&mut state, Duration::from_millis(1));

        let expected_speed = ability.initial_projectile_speed + ability.scaled_projectile_speed;
        let velocities = state.ecs().read_storage::<Vel>();
        let vel = velocities.get(item_entity).expect("item lost its Vel");
        // Check the horizontal (xy) speed rather than the full 3D magnitude:
        // gravity has had a tick to nudge in a small z-component by the time this
        // reads, which isn't part of what the throw formula controls, and the
        // caster's default look direction (used as the throw direction) isn't
        // pinned to a specific axis here.
        assert!(
            (vel.0.xy().magnitude() - expected_speed).abs() < 0.5,
            "expected throw speed ~{expected_speed} horizontally, got {:?}",
            vel.0
        );
    }

    /// (4) A thrown item that hits a creature registers a hit and the target
    /// takes the expected `CombatEffect::Knockback` + damage.
    ///
    /// Asserts on the `HealthChangeEvent`/`KnockbackEvent` the shared
    /// collision/combat pipeline (`phys::Sys` + `projectile::Sys`, both
    /// wired in by `add_local_systems`) emits for the struck creature,
    /// rather than the `Health`/`Vel` components actually changing —
    /// applying those events to components is server-only wiring
    /// (`server/src/events/entity_manipulation.rs`) outside what a
    /// `xindeler-common-systems` test can reach, and is itself
    /// pre-existing, already-relied-upon behavior (`Throw` goes through the
    /// exact same event pair). What this test is actually proving is that
    /// `TelekineticGrip`'s release-as-throw correctly wires the item into
    /// that shared pipeline in the first place.
    #[test]
    fn thrown_item_hits_creature() {
        let mut state = setup();
        let (_item_entity, item_uid) = create_item(&mut state, Vec3::new(1.0, 0.0, 0.0));
        let (creature_entity, _creature_uid) =
            create_creature(&mut state, Vec3::new(2.0, 0.0, 0.0));
        let ability = default_ability(Some(item_uid));
        let caster = create_caster(&mut state, ability.clone());
        set_held(&mut state, caster, &ability, true);

        complete_buildup(&mut state, &ability);

        // Aim the throw straight at the creature.
        {
            let mut controllers = state.ecs().write_storage::<Controller>();
            controllers.get_mut(caster).unwrap().inputs.look_dir =
                Dir::from_unnormalized(Vec3::new(1.0, 0.0, 0.0)).unwrap();
        }

        // Hold well past `place_threshold` so this reads as a throw, not a place
        // (mirrors `release_as_throw`).
        let hold_ticks: u32 = 30;
        let dt = ability.charge_duration / hold_ticks;
        for _ in 0..hold_ticks {
            tick(&mut state, dt);
        }

        set_held(&mut state, caster, &ability, false);
        tick(&mut state, Duration::from_millis(1));

        // Give the physics/projectile pipeline a few ticks to register the hit,
        // draining the combat event busses each tick so a hit isn't missed
        // regardless of exactly which tick it lands on.
        let mut health_changes = Vec::new();
        let mut knockbacks = Vec::new();
        for _ in 0..60 {
            tick(&mut state, Duration::from_millis(16));
            health_changes.extend(
                state
                    .ecs()
                    .read_resource::<EventBus<HealthChangeEvent>>()
                    .recv_all(),
            );
            knockbacks.extend(
                state
                    .ecs()
                    .read_resource::<EventBus<KnockbackEvent>>()
                    .recv_all(),
            );
        }

        let damage = health_changes
            .iter()
            .find(|ev| ev.entity == creature_entity)
            .unwrap_or_else(|| {
                panic!("expected a HealthChangeEvent for the struck creature, got none")
            });
        assert!(
            damage.change.amount < 0.0,
            "expected the creature to take damage, got a health change of {}",
            damage.change.amount
        );

        let knockback = knockbacks
            .iter()
            .find(|ev| ev.entity == creature_entity)
            .unwrap_or_else(|| {
                panic!("expected a KnockbackEvent for the struck creature, got none")
            });
        assert!(
            knockback.impulse.magnitude_squared() > 0.0,
            "expected a nonzero knockback impulse, got {:?}",
            knockback.impulse
        );
    }
}
