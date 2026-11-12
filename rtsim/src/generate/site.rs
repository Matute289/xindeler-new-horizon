use crate::data::{FactionId, Factions, Site};
use common::store::Id;
use rand::prelude::*;
use vek::*;
use world::{
    IndexRef, World,
    site::{Site as WorldSite, SiteKind},
};

/// Classifies a world site as Good (`Some(true)`), Evil (`Some(false)`) or
/// Neutral/unaligned (`None`) for rtsim faction assignment purposes.
///
/// `is_authored_settlement` is `site::Site::is_authored_settlement`: whether
/// this site was established from a designer-authored settlement pin (e.g. a
/// Cromatolis inn/post) rather than placed by procedural civilisation
/// simulation. It exists because `Camp` is reused as a physical stand-in for
/// authored settlement categories that don't have a dedicated building
/// generator yet (see that field's doc) -- those are not the same thing as a
/// genuine wild procedural bandit camp, so they don't inherit `Camp`'s Evil
/// classification below. Neutral (no faction) is more honest than Good here,
/// since a peaceful default was never actually designed for them.
// TODO: This is stupid, do better
fn good_or_evil(kind: Option<&SiteKind>, is_authored_settlement: bool) -> Option<bool> {
    match kind {
        // Good
        Some(
            SiteKind::Refactor
            | SiteKind::CliffTown
            | SiteKind::DesertCity
            | SiteKind::SavannahTown
            | SiteKind::CoastalTown
            | SiteKind::Citadel,
        ) => Some(true),
        Some(SiteKind::Camp) if is_authored_settlement => None,
        // Evil
        Some(
            SiteKind::Myrmidon
            | SiteKind::ChapelSite
            | SiteKind::Terracotta
            | SiteKind::Gnarling
            | SiteKind::Cultist
            | SiteKind::Sahagin
            | SiteKind::PirateHideout
            | SiteKind::JungleRuin
            | SiteKind::RockCircle
            | SiteKind::TrollCave
            | SiteKind::Camp
            | SiteKind::Haniwa
            | SiteKind::Adlet
            | SiteKind::VampireCastle
            | SiteKind::DwarvenMine,
        ) => Some(false),
        // Neutral
        Some(
            SiteKind::GiantTree
            | SiteKind::GliderCourse
            | SiteKind::Bridge(..)
            | SiteKind::Fortification(..),
        )
        | None => None,
    }
}

impl Site {
    pub fn generate(
        world_site_id: Id<WorldSite>,
        _world: &World,
        index: IndexRef,
        nearby_factions: &[(Vec2<i32>, FactionId)],
        factions: &Factions,
        rng: &mut impl Rng,
    ) -> Self {
        let world_site = index.sites.get(world_site_id);
        let wpos = world_site.origin;

        let good_or_evil =
            good_or_evil(world_site.kind.as_ref(), world_site.is_authored_settlement);

        Self {
            seed: rng.random(),
            wpos,
            world_site: Some(world_site_id),
            faction: good_or_evil.and_then(|good_or_evil| {
                nearby_factions
                    .iter()
                    .filter(|(_, faction)| {
                        factions
                            .get(*faction)
                            .is_some_and(|f| f.good_or_evil == good_or_evil)
                    })
                    .min_by_key(|(faction_wpos, _)| {
                        faction_wpos
                            .as_::<i64>()
                            .distance_squared(wpos.as_::<i64>())
                    })
                    .map(|(_, faction)| *faction)
            }),
            count_loaded_chunks: 0,
            population: Default::default(),
            known_reports: Default::default(),
            nearby_sites_by_size: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::good_or_evil;
    use world::site::SiteKind;

    #[test]
    fn authored_camp_stand_in_is_neutral_not_evil() {
        // An authored Cromatolis inn/post resolves to `SiteKind::Camp` as a
        // physical stand-in (see `site::Site::is_authored_settlement`'s
        // doc) -- it must not be classified Evil like a genuine bandit camp.
        assert_eq!(good_or_evil(Some(&SiteKind::Camp), true), None);
    }

    #[test]
    fn wild_camp_is_still_evil() {
        // A genuine wild procedural `Camp` (no authored-settlement context)
        // must keep its existing Evil classification.
        assert_eq!(good_or_evil(Some(&SiteKind::Camp), false), Some(false));
    }

    #[test]
    fn good_and_neutral_kinds_are_unaffected_by_the_authored_flag() {
        // `is_authored_settlement` only changes anything for `Camp` --
        // sanity check a couple of the other arms stay put regardless.
        assert_eq!(good_or_evil(Some(&SiteKind::Refactor), true), Some(true));
        assert_eq!(good_or_evil(Some(&SiteKind::Refactor), false), Some(true));
        assert_eq!(good_or_evil(Some(&SiteKind::GiantTree), true), None);
        assert_eq!(good_or_evil(None, true), None);
    }
}
