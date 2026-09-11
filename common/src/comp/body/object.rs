use crate::{
    comp::{Density, Mass},
    consts::{AIR_DENSITY, IRON_DENSITY, WATER_DENSITY},
};
use common_base::enum_iter;
use rand::{prelude::IndexedRandom, rng};
use serde::{Deserialize, Serialize};
use vek::Vec3;

enum_iter! {
    ~const_array(ALL)
    #[derive(
        Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
    )]
    #[repr(u32)]
    pub enum Body {
        Arrow = 0,
        Bomb = 1,
        Scarecrow = 2,
        Pumpkin = 3,
        Campfire = 4,
        BoltFire = 5,
        ArrowSnake = 6,
        CampfireLit = 7,
        BoltFireBig = 8,
        TrainingDummy = 9,
        FireworkBlue = 10,
        FireworkGreen = 11,
        FireworkPurple = 12,
        FireworkRed = 13,
        FireworkWhite = 14,
        FireworkYellow = 15,
        MultiArrow = 16,
        BoltNature = 17,
        ToughMeat = 18,
        BeastMeat = 19,
        Crossbow = 20,
        ArrowTurret = 21,
        ClayRocket = 22,
        HaniwaSentry = 23,
        SeaLantern = 24,
        Snowball = 25,
        Tornado = 26,
        Apple = 27,
        Hive = 28,
        Coconut = 29,
        SpitPoison = 30,
        BoltIcicle = 31,
        Dart = 32,
        GnarlingTotemRed = 33,
        GnarlingTotemGreen = 34,
        GnarlingTotemWhite = 35,
        DagonBomb = 36,
        BarrelOrgan = 37,
        IceBomb = 38,
        SpectralSwordSmall = 39,
        SpectralSwordLarge = 40,
        LaserBeam = 41,
        AdletSpear = 42,
        AdletTrap = 43,
        Flamethrower = 44,
        Mine = 45,
        LightningBolt = 46,
        SpearIcicle = 47,
        Portal = 48,
        PortalActive = 49,
        FieryTornado = 50,
        FireRainDrop = 51,
        ArrowClay = 52,
        GrenadeClay = 53,
        Pebble = 54,
        LaserBeamSmall = 55,
        TerracottaStatue = 56,
        TerracottaDemolisherBomb = 57,
        BoltBesieger = 58,
        SurpriseEgg = 59,
        BubbleBomb = 60,
        IronPikeBomb = 61,
        Lavathrower = 62,
        PoisonBall = 63,
        StrigoiHead = 64,
        HarlequinDagger = 65,
        BloodBomb = 66,
        MinotaurAxe = 67,
        BorealTrap = 68,
        Crux = 69,
        ArrowHeavy = 70,
        FireRing = 71,
        PyroclasmBolt = 72,
        NapalmShot = 73,
        NapalmPool = 74,
        ThornStake = 75,
        /// A static sensor spawned by a remote-sensing spell at a validated
        /// world point. `ConcealedUnlessTrueSight`-gated on the render path:
        /// invisible on the normal path, drawn plainly for a True-Sight
        /// holder (`common/src/comp/detection.rs`).
        RemoteSensor = 76,
        /// `arcane_eye`'s piloted sensor: the same `ConcealedUnlessTrueSight`
        /// gating and 30 HP pool as `RemoteSensor`, plus a `Controller` so
        /// its caster can fly it. A distinct variant (not a reuse of
        /// `RemoteSensor`) because it carries `Vel`/`Ori`/`Controller` and is
        /// driven by `common/systems/src/pilot.rs`, unlike the static sensor.
        ArcaneEye = 77,
        /// An articulated Cromatolis Aerial Citadel defence cannon, aimed by
        /// `common::comp::citadel::{desired_citadel_turret_angles,
        /// update_citadel_turret}`. Fires the harmless practice beam used
        /// while the cannon is unmanned or in drill mode.
        CitadelArcaneCannon = 78,
        /// Same turret footprint as `CitadelArcaneCannon`, mounted with the
        /// sphere-barrel model for cannons that fire the practice sphere
        /// instead of the practice beam.
        CitadelArcaneSphereCannon = 79,
        /// The harmless Cromatolis practice sphere fired by
        /// `CitadelArcaneSphereCannon`, timed by
        /// `common::comp::citadel::CitadelPracticeSphere`.
        LaserSphereLarge = 80,
        /// The harmless Cromatolis practice beam fired by
        /// `CitadelArcaneCannon`, timed by
        /// `common::comp::citadel::CitadelPracticeBeam`. Half a metre wide,
        /// matching `CitadelPracticeBeam::diameter`.
        LaserBeamLarge = 81,
        /// A smaller Cromatolis practice sphere, sharing the same voxel
        /// model as `LaserBeamSmall` (see `object_manifest.ron`).
        LaserSphereSmall = 82,
    }
}

impl Body {
    pub fn random() -> Self {
        let mut rng = rng();
        *ALL_OBJECTS.choose(&mut rng).unwrap()
    }
}

pub const ALL_OBJECTS: [Body; Body::NUM_KINDS] = Body::ALL;

impl From<Body> for super::Body {
    fn from(body: Body) -> Self { super::Body::Object(body) }
}

impl Body {
    pub fn to_string(self) -> &'static str {
        match self {
            Body::Arrow => "arrow",
            Body::Bomb => "bomb",
            Body::Scarecrow => "scarecrow",
            Body::Pumpkin => "pumpkin",
            Body::Campfire => "campfire",
            Body::CampfireLit => "campfire_lit",
            Body::BoltFire => "bolt_fire",
            Body::BoltFireBig => "bolt_fire_big",
            Body::ArrowSnake => "arrow_snake",
            Body::TrainingDummy => "training_dummy",
            Body::FireworkBlue => "firework_blue",
            Body::FireworkGreen => "firework_green",
            Body::FireworkPurple => "firework_purple",
            Body::FireworkRed => "firework_red",
            Body::FireworkWhite => "firework_white",
            Body::FireworkYellow => "firework_yellow",
            Body::MultiArrow => "multi_arrow",
            Body::BoltNature => "bolt_nature",
            Body::ToughMeat => "tough_meat",
            Body::BeastMeat => "beast_meat",
            Body::Crossbow => "crossbow",
            Body::ArrowTurret => "arrow_turret",
            Body::ClayRocket => "clay_rocket",
            Body::HaniwaSentry => "haniwa_sentry",
            Body::SeaLantern => "sea_lantern",
            Body::Snowball => "snowball",
            Body::Tornado => "tornado",
            Body::Apple => "apple",
            Body::Hive => "hive",
            Body::Coconut => "coconut",
            Body::SpitPoison => "spit_poison",
            Body::BoltIcicle => "bolt_icicle",
            Body::Dart => "dart",
            Body::GnarlingTotemRed => "gnarling_totem_red",
            Body::GnarlingTotemGreen => "gnarling_totem_green",
            Body::GnarlingTotemWhite => "gnarling_totem_white",
            Body::DagonBomb => "dagon_bomb",
            Body::TerracottaDemolisherBomb => "terracotta_demolisher_bomb",
            Body::BarrelOrgan => "barrel_organ",
            Body::IceBomb => "ice_bomb",
            Body::SpectralSwordSmall => "spectral_sword_small",
            Body::SpectralSwordLarge => "spectral_sword_large",
            Body::LaserBeam => "laser_beam",
            Body::LaserBeamSmall => "laser_beam_small",
            Body::AdletSpear => "adlet_spear",
            Body::AdletTrap => "adlet_trap",
            Body::Flamethrower => "flamethrower",
            Body::Lavathrower => "lavathrower",
            Body::Mine => "mine",
            Body::LightningBolt => "lightning_bolt",
            Body::SpearIcicle => "spear_icicle",
            Body::Portal => "portal",
            Body::PortalActive => "portal_active",
            Body::FieryTornado => "fiery_tornado",
            Body::FireRainDrop => "fire_rain_drop",
            Body::ArrowClay => "arrow_clay",
            Body::GrenadeClay => "grenade_clay",
            Body::Pebble => "pebble",
            Body::TerracottaStatue => "terracotta_statue",
            Body::BoltBesieger => "besieger_bolt",
            Body::SurpriseEgg => "surprise_egg",
            Body::BubbleBomb => "bubble_bomb",
            Body::IronPikeBomb => "iron_pike_bomb",
            Body::PoisonBall => "poison_ball",
            Body::StrigoiHead => "strigoi_head",
            Body::HarlequinDagger => "harlequin_dagger",
            Body::BloodBomb => "blood_bomb",
            Body::MinotaurAxe => "minotaur_axe",
            Body::BorealTrap => "boreal_trap",
            Body::Crux => "crux",
            Body::ArrowHeavy => "heavy_arrow",
            Body::FireRing => "fire_ring",
            Body::PyroclasmBolt => "pyroclasm_bolt",
            Body::NapalmShot => "napalm_shot",
            Body::NapalmPool => "napalm_pool",
            Body::ThornStake => "thorn_stake",
            Body::RemoteSensor => "remote_sensor",
            Body::ArcaneEye => "arcane_eye",
            Body::CitadelArcaneCannon => "citadel_arcane_cannon",
            Body::CitadelArcaneSphereCannon => "citadel_arcane_sphere_cannon",
            Body::LaserSphereLarge => "laser_sphere_large",
            Body::LaserBeamLarge => "laser_beam_large",
            Body::LaserSphereSmall => "laser_sphere_small",
        }
    }

    pub fn density(&self) -> Density {
        let density = match self {
            Body::Arrow
            | Body::ArrowSnake
            | Body::ArrowTurret
            | Body::MultiArrow
            | Body::ArrowClay
            | Body::BoltBesieger
            | Body::Dart
            | Body::DagonBomb
            | Body::TerracottaDemolisherBomb
            | Body::SpectralSwordSmall
            | Body::SpectralSwordLarge
            | Body::AdletSpear
            | Body::HarlequinDagger
            | Body::AdletTrap
            | Body::Flamethrower
            | Body::Lavathrower
            | Body::BorealTrap
            | Body::BloodBomb
            | Body::ArrowHeavy
            | Body::ThornStake => 500.0,
            Body::Bomb | Body::Mine | Body::SurpriseEgg => 2000.0, /* I have no idea what it's */
            // supposed to be
            Body::Scarecrow => 900.0,
            Body::TrainingDummy => 2000.0,
            Body::Snowball => 0.9 * WATER_DENSITY,
            Body::Pebble => 1000.0,
            Body::Crux
            | Body::FireRing
            | Body::PyroclasmBolt
            | Body::RemoteSensor
            | Body::ArcaneEye => AIR_DENSITY,
            // let them sink
            _ => 1.1 * WATER_DENSITY,
        };

        Density(density)
    }

    pub fn mass(&self) -> Mass {
        let m = match self {
            // MultiArrow is one of several arrows, not several arrows combined
            Body::Arrow | Body::ArrowSnake | Body::ArrowTurret | Body::MultiArrow | Body::Dart => {
                0.003
            },
            Body::SpectralSwordSmall => 0.5,
            Body::SpectralSwordLarge => 50.0,
            Body::BoltFire
            | Body::BoltFireBig
            | Body::BoltNature
            | Body::BoltIcicle
            | Body::FireRainDrop
            | Body::ArrowClay
            | Body::Pebble
            | Body::BubbleBomb
            | Body::IronPikeBomb
            | Body::BoltBesieger
            | Body::PoisonBall
            | Body::ArrowHeavy
            | Body::FireRing
            | Body::PyroclasmBolt
            | Body::NapalmShot
            | Body::ThornStake => 1.0,
            Body::SpitPoison => 100.0,
            Body::Bomb
            | Body::DagonBomb
            | Body::SurpriseEgg
            | Body::TerracottaDemolisherBomb
            | Body::BloodBomb => {
                0.5 * IRON_DENSITY * std::f32::consts::PI / 6.0 * self.dimensions().x.powi(3)
            },
            Body::Campfire
            | Body::CampfireLit
            | Body::BarrelOrgan
            | Body::TerracottaStatue
            | Body::NapalmPool => 300.0,
            Body::Crossbow => 200.0,
            Body::Flamethrower | Body::Lavathrower => 200.0,
            Body::FireworkBlue
            | Body::FireworkGreen
            | Body::FireworkPurple
            | Body::FireworkRed
            | Body::FireworkWhite
            | Body::FireworkYellow => 1.0,
            Body::ToughMeat => 50.0,
            Body::BeastMeat => 50.0,
            Body::Pumpkin | Body::StrigoiHead => 10.0,
            Body::Scarecrow => 50.0,
            Body::TrainingDummy => 60.0,
            Body::ClayRocket | Body::GrenadeClay => 50.0,
            Body::HaniwaSentry => 300.0,
            Body::SeaLantern => 1000.0,
            Body::MinotaurAxe => 100000.0,
            Body::Snowball => 7360.0, // 2.5 m diamter
            Body::Tornado | Body::FieryTornado => 50.0,
            Body::Apple => 2.0,
            Body::Hive => 2.0,
            Body::Coconut => 2.0,
            Body::GnarlingTotemRed | Body::GnarlingTotemGreen | Body::GnarlingTotemWhite => 100.0,
            Body::IceBomb => 12298.0, // 2.5 m diamter but ice
            Body::LaserBeam | Body::LaserBeamSmall => 80000.0,
            Body::AdletSpear => 1.5,
            Body::AdletTrap => 10.0,
            Body::Mine => 100.0,
            Body::HarlequinDagger => 1.5,
            Body::BorealTrap => 10.0,
            Body::LightningBolt | Body::SpearIcicle => 20000.0,
            Body::Portal | Body::PortalActive => 10.0, // I dont know really
            Body::Crux => 100.0,
            Body::RemoteSensor => 1.0,
            Body::ArcaneEye => 1.0,
            // The articulated citadel cannons render at 2.5x their source
            // voxels; mass follows that scaled physical volume.
            Body::CitadelArcaneCannon | Body::CitadelArcaneSphereCannon => 54_687.5,
            Body::LaserSphereLarge | Body::LaserBeamLarge | Body::LaserSphereSmall => 80000.0,
        };

        Mass(m)
    }

    pub fn dimensions(&self) -> Vec3<f32> {
        match self {
            Body::Arrow
            | Body::ArrowSnake
            | Body::MultiArrow
            | Body::ArrowTurret
            | Body::ArrowClay
            | Body::BoltBesieger
            | Body::Dart
            | Body::HarlequinDagger
            | Body::AdletSpear => Vec3::new(0.01, 0.8, 0.01),
            Body::AdletTrap => Vec3::new(1.0, 0.6, 0.3),
            Body::BoltFire | Body::PoisonBall | Body::NapalmShot => Vec3::new(0.1, 0.1, 0.1),
            Body::SpectralSwordSmall => Vec3::new(0.2, 0.9, 0.1),
            Body::SpectralSwordLarge => Vec3::new(0.2, 1.5, 0.1),
            Body::Crossbow => Vec3::new(3.0, 3.0, 1.5),
            Body::Flamethrower => Vec3::new(3.0, 3.0, 2.5),
            Body::Lavathrower => Vec3::new(3.0, 3.0, 2.0),
            Body::HaniwaSentry => Vec3::new(0.8, 0.8, 1.4),
            Body::SeaLantern => Vec3::new(0.8, 0.8, 1.4),
            Body::Snowball => Vec3::broadcast(2.5),
            Body::Tornado | Body::FieryTornado => Vec3::new(2.0, 2.0, 3.4),
            Body::TrainingDummy => Vec3::new(1.5, 1.5, 3.0),
            Body::BorealTrap => Vec3::new(1.0, 0.6, 0.3),
            Body::GnarlingTotemRed | Body::GnarlingTotemGreen | Body::GnarlingTotemWhite => {
                Vec3::new(0.8, 0.8, 1.4)
            },
            Body::BarrelOrgan => Vec3::new(4.0, 2.0, 3.0),
            Body::TerracottaStatue => Vec3::new(5.0, 5.0, 5.0),
            Body::IceBomb => Vec3::broadcast(2.5),
            Body::LaserBeam => Vec3::new(8.0, 8.0, 8.0),
            Body::LaserBeamSmall => Vec3::new(1.0, 1.0, 1.0),
            Body::Mine => Vec3::new(0.8, 0.8, 0.5),
            Body::LightningBolt | Body::SpearIcicle => Vec3::new(1.0, 1.0, 1.0),
            Body::FireRainDrop => Vec3::new(0.01, 0.01, 0.02),
            Body::Pebble => Vec3::new(0.4, 0.4, 0.4),
            Body::MinotaurAxe => Vec3::new(5.0, 5.0, 5.0),
            Body::Crux => Vec3::new(2.0, 2.0, 2.0),
            Body::ArrowHeavy | Body::ThornStake => Vec3::new(0.1, 0.9, 0.1),
            Body::CitadelArcaneCannon | Body::CitadelArcaneSphereCannon => {
                Vec3::new(9.0, 15.0, 6.25)
            },
            Body::LaserBeamLarge | Body::LaserSphereLarge => Vec3::broadcast(0.5),
            Body::LaserSphereSmall => Vec3::broadcast(0.08),
            // FIXME: this *must* be exhaustive match
            _ => Vec3::broadcast(0.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Body;
    use std::collections::HashSet;

    /// The Cromatolis Aerial Citadel (COW-8) added 5 new `Body::Object`
    /// variants at fresh discriminants (78..=82), after this repo's own
    /// prior work had already claimed 75..=77 for `ThornStake`/
    /// `RemoteSensor`/`ArcaneEye`. This guards against any of the 5 new
    /// IDs colliding with those or any other existing variant.
    #[test]
    fn new_citadel_and_laser_body_ids_do_not_collide_with_any_existing_variant() {
        let new_ids = [
            (Body::CitadelArcaneCannon, "CitadelArcaneCannon"),
            (Body::CitadelArcaneSphereCannon, "CitadelArcaneSphereCannon"),
            (Body::LaserSphereLarge, "LaserSphereLarge"),
            (Body::LaserBeamLarge, "LaserBeamLarge"),
            (Body::LaserSphereSmall, "LaserSphereSmall"),
        ];

        let mut seen = HashSet::new();
        for body in Body::ALL {
            assert!(
                seen.insert(body as u32),
                "duplicate Body::Object discriminant {} ({:?})",
                body as u32,
                body,
            );
        }

        for (body, name) in new_ids {
            assert!(
                (body as u32) >= 78,
                "{name} must use a fresh discriminant starting at 78, got {}",
                body as u32,
            );
        }

        assert_eq!(Body::ThornStake as u32, 75);
        assert_eq!(Body::RemoteSensor as u32, 76);
        assert_eq!(Body::ArcaneEye as u32, 77);
    }

    #[test]
    fn existing_upstream_laser_beam_bodies_are_untouched() {
        assert_eq!(Body::LaserBeam as u32, 41);
        assert_eq!(Body::LaserBeam.to_string(), "laser_beam");
        assert_eq!(Body::LaserBeamSmall as u32, 55);
        assert_eq!(Body::LaserBeamSmall.to_string(), "laser_beam_small");
    }

    #[test]
    fn citadel_arcane_cannon_has_a_stable_asset_name_and_physical_envelope() {
        use vek::Vec3;

        assert_eq!(
            Body::CitadelArcaneCannon.to_string(),
            "citadel_arcane_cannon"
        );
        assert_eq!(
            Body::CitadelArcaneCannon.dimensions(),
            Vec3::new(9.0, 15.0, 6.25)
        );
    }

    #[test]
    fn citadel_arcane_sphere_cannon_reuses_the_turret_footprint() {
        assert_eq!(
            Body::CitadelArcaneSphereCannon.to_string(),
            "citadel_arcane_sphere_cannon"
        );
        assert_eq!(
            Body::CitadelArcaneSphereCannon.dimensions(),
            Body::CitadelArcaneCannon.dimensions()
        );
    }
}
