//! The authored settlement layouts layer
//! (`world.map.cromatolis_v0_settlement_layouts`): per settlement, a frame, a
//! main-plaza anchor, the wards it may build in with a building target each,
//! and keep-out circles. Resolved here into [`SettlementLayout`]s that
//! `Site::generate_city` consumes; a settlement without an entry generates
//! exactly as before.
//!
//! Like every other authored Cromatolis layer, a missing, unparsable or
//! invalid file stops world generation (see `load_authored_cromatolis_layer`).

use super::{AuthoredCromatolisLandmarks, AuthoredCromatolisSettlements};
use crate::site::layout::{
    FieldPlacement, LayoutExclusion, LayoutFrame, LayoutWard, MAX_WARDS, SettlementLayout,
    WaterSide,
};
use common::{
    assets::{BoxedError, FileAsset, load_ron},
    terrain::{CoordinateConversions, MapSizeLg},
};
use serde::Deserialize;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};
use vek::*;

pub(super) const SPECIFIER: &str = "world.map.cromatolis_v0_settlement_layouts";
const EXPECTED_SCHEMA: &str = "xindeler.settlement_layouts.v1";
/// Widest rural ring around a footprint, in tiles.
const MAX_FIELD_RING_TILES: u32 = 16;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthoredSettlementLayouts {
    schema: String,
    layouts: Vec<AuthoredSettlementLayout>,
}

impl FileAsset for AuthoredSettlementLayouts {
    const EXTENSION: &'static str = "ron";

    fn from_bytes(bytes: Cow<[u8]>) -> Result<Self, BoxedError> { load_ron(&bytes) }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredSettlementLayout {
    /// The authored settlement (`cromatolis_v0_sites`) this layout is for.
    site_id: String,
    frame: AuthoredFrame,
    plan: AuthoredPlan,
    /// Keep-out circles around other authored settlements or landmarks, in
    /// blocks from their pin. Every authored landmark's own footprint is
    /// excluded automatically.
    #[serde(default)]
    exclude_sites: Vec<AuthoredExclusion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredFrame {
    origin: AuthoredFrameOrigin,
    /// The bank direction, degrees counter-clockwise from east.
    along_deg: f64,
    water_side: AuthoredWaterSide,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum AuthoredFrameOrigin {
    /// The settlement's own pin (its site origin).
    Pin,
    /// A world position in blocks (x east, y north).
    Wpos(i32, i32),
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum AuthoredWaterSide {
    Left,
    Right,
}

#[derive(Debug, Deserialize)]
enum AuthoredPlan {
    /// Rectangles in the frame, each with its own building target.
    Wards(AuthoredWardPlan),
    /// One rectangle around the frame origin, given as how far the town may
    /// reach in each direction; its target is the settlement's own.
    Sectors(AuthoredSectorPlan),
    /// Another settlement's `Wards`/`Sectors` plan, laid out in this
    /// settlement's own frame (opposite water sides mirror it).
    MirrorOf(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredWardPlan {
    anchor: AuthoredFramePoint,
    #[serde(default)]
    plaza_radius: Option<u32>,
    fields: AuthoredFields,
    wards: Vec<AuthoredWard>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredSectorPlan {
    anchor: AuthoredFramePoint,
    #[serde(default)]
    plaza_radius: Option<u32>,
    fields: AuthoredFields,
    /// Blocks along the bank direction.
    front: f64,
    /// Blocks against the bank direction.
    back: f64,
    /// Blocks to the left of the bank direction.
    left: f64,
    /// Blocks to the right of the bank direction.
    right: f64,
}

/// Frame coordinates: `a` blocks toward the water, `b` blocks along the bank.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredFramePoint {
    a: f64,
    b: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum AuthoredFields {
    Inside,
    Ring(u32),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredWard {
    id: String,
    a: (f64, f64),
    b: (f64, f64),
    target_buildings: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredExclusion {
    site_id: String,
    radius_blocks: i32,
}

/// What validation and resolution need to know about the other authored
/// layers: every settlement's pin and target, every landmark's pin.
pub(super) struct AuthoredPins {
    /// Settlement id -> (pin in world blocks, target buildings).
    settlements: HashMap<String, (Vec2<i32>, usize)>,
    /// Landmark id -> pin in world blocks.
    landmarks: HashMap<String, Vec2<i32>>,
}

impl AuthoredPins {
    pub(super) fn new(
        map_size: MapSizeLg,
        settlements: &AuthoredCromatolisSettlements,
        landmarks: &AuthoredCromatolisLandmarks,
    ) -> Self {
        let pin = |chunk: Vec2<i32>| chunk.cpos_to_wpos_center();
        Self {
            settlements: settlements
                .settlements
                .iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        (pin(s.center.to_chunk_pos(map_size)), s.target_buildings),
                    )
                })
                .collect(),
            landmarks: landmarks
                .landmarks
                .iter()
                .map(|l| (l.id.clone(), pin(l.center.to_chunk_pos(map_size))))
                .collect(),
        }
    }

    fn pin_of(&self, id: &str) -> Option<Vec2<i32>> {
        self.settlements
            .get(id)
            .map(|(pin, _)| *pin)
            .or_else(|| self.landmarks.get(id).copied())
    }
}

/// A plan with its source settlement's anchor, plaza and field settings, as
/// the settlement that uses it (possibly through `MirrorOf`) sees it.
struct PlanParts<'a> {
    anchor: AuthoredFramePoint,
    plaza_radius: Option<u32>,
    fields: AuthoredFields,
    wards: WardsSource<'a>,
}

enum WardsSource<'a> {
    Wards(&'a [AuthoredWard]),
    Sectors(&'a AuthoredSectorPlan),
}

impl AuthoredSettlementLayouts {
    /// Checks everything that can be checked without generating a town;
    /// the error names the layout and the reason.
    pub(super) fn validate(&self, pins: &AuthoredPins) -> Result<(), String> {
        if self.schema != EXPECTED_SCHEMA {
            return Err(format!(
                "expected schema {EXPECTED_SCHEMA}, got {}",
                self.schema
            ));
        }
        let mut seen = HashSet::new();
        for layout in &self.layouts {
            let id = layout.site_id.as_str();
            if !seen.insert(id) {
                return Err(format!("{id} has more than one layout"));
            }
            self.validate_one(layout, pins)
                .map_err(|reason| format!("layout {id}: {reason}"))?;
        }
        Ok(())
    }

    fn validate_one(
        &self,
        layout: &AuthoredSettlementLayout,
        pins: &AuthoredPins,
    ) -> Result<(), String> {
        let Some(&(pin, target)) = pins.settlements.get(&layout.site_id) else {
            return Err("no authored settlement has this id".to_string());
        };
        if !layout.frame.along_deg.is_finite() {
            return Err("frame along_deg is not a finite number".to_string());
        }
        for exclusion in &layout.exclude_sites {
            if pins.pin_of(&exclusion.site_id).is_none() {
                return Err(format!(
                    "exclude_sites names {}, which is no authored settlement or landmark",
                    exclusion.site_id
                ));
            }
            if exclusion.site_id == layout.site_id {
                return Err("exclude_sites names the settlement itself".to_string());
            }
            if exclusion.radius_blocks <= 0 {
                return Err(format!(
                    "exclude_sites {} has a radius of {} blocks",
                    exclusion.site_id, exclusion.radius_blocks
                ));
            }
        }
        let parts = self.plan_parts(layout)?;
        if let Some(radius) = parts.plaza_radius
            && !(1..=3).contains(&radius)
        {
            return Err(format!("plaza_radius {radius} is outside 1..=3"));
        }
        if let AuthoredFields::Ring(width) = parts.fields
            && !(1..=MAX_FIELD_RING_TILES).contains(&width)
        {
            return Err(format!(
                "fields Ring({width}) is outside 1..={MAX_FIELD_RING_TILES}"
            ));
        }
        for value in [parts.anchor.a, parts.anchor.b] {
            if !value.is_finite() {
                return Err("the anchor is not finite".to_string());
            }
        }
        match parts.wards {
            WardsSource::Wards(wards) => {
                if wards.is_empty() || wards.len() > MAX_WARDS {
                    return Err(format!(
                        "{} wards; a plan needs 1..={MAX_WARDS}",
                        wards.len()
                    ));
                }
                let mut ids = HashSet::new();
                for ward in wards {
                    if ward.id.is_empty() || !ids.insert(ward.id.as_str()) {
                        return Err(format!("duplicate or empty ward id {:?}", ward.id));
                    }
                    for (axis, range) in [("a", ward.a), ("b", ward.b)] {
                        if !(range.0.is_finite() && range.1.is_finite() && range.0 < range.1) {
                            return Err(format!(
                                "ward {} has an empty or non-finite {axis} range {range:?}",
                                ward.id
                            ));
                        }
                    }
                    if ward.target_buildings == 0 {
                        return Err(format!("ward {} has a target of 0 buildings", ward.id));
                    }
                }
                for (i, x) in wards.iter().enumerate() {
                    for y in &wards[i + 1..] {
                        let overlaps = |p: (f64, f64), q: (f64, f64)| p.0.max(q.0) < p.1.min(q.1);
                        if overlaps(x.a, y.a) && overlaps(x.b, y.b) {
                            return Err(format!("wards {} and {} overlap", x.id, y.id));
                        }
                    }
                }
                let sum: usize = wards.iter().map(|ward| ward.target_buildings).sum();
                if sum != target {
                    return Err(format!(
                        "the ward targets add up to {sum}, but the settlement's target_buildings \
                         is {target}"
                    ));
                }
            },
            WardsSource::Sectors(sectors) => {
                for (name, value) in [
                    ("front", sectors.front),
                    ("back", sectors.back),
                    ("left", sectors.left),
                    ("right", sectors.right),
                ] {
                    if !(value.is_finite() && value >= 0.0) {
                        return Err(format!("sector {name} is {value}"));
                    }
                }
                if sectors.front + sectors.back <= 0.0 || sectors.left + sectors.right <= 0.0 {
                    return Err("the sectors enclose no area".to_string());
                }
            },
        }

        // The anchor must lie in a ward, inside the tiles the generator
        // evaluates around the pin.
        let resolved = self.resolve_one(layout, pins, &[])?;
        let anchor = resolved.anchor_wpos();
        if !resolved
            .wards
            .iter()
            .any(|ward| ward.contains(resolved.anchor))
        {
            return Err(format!(
                "the anchor (a {}, b {}) is in no ward",
                resolved.anchor.x, resolved.anchor.y
            ));
        }
        if !resolved.anchor_in_domain(pin) {
            return Err(format!(
                "the anchor at world ({:.0}, {:.0}) is more than {} tiles from the settlement's \
                 pin ({}, {})",
                anchor.x,
                anchor.y,
                resolved.domain_tiles(pin),
                pin.x,
                pin.y
            ));
        }
        Ok(())
    }

    fn plan_parts<'a>(
        &'a self,
        layout: &'a AuthoredSettlementLayout,
    ) -> Result<PlanParts<'a>, String> {
        match &layout.plan {
            AuthoredPlan::Wards(plan) => Ok(PlanParts {
                anchor: plan.anchor,
                plaza_radius: plan.plaza_radius,
                fields: plan.fields,
                wards: WardsSource::Wards(&plan.wards),
            }),
            AuthoredPlan::Sectors(plan) => Ok(PlanParts {
                anchor: plan.anchor,
                plaza_radius: plan.plaza_radius,
                fields: plan.fields,
                wards: WardsSource::Sectors(plan),
            }),
            AuthoredPlan::MirrorOf(source) => {
                let source_layout = self
                    .layouts
                    .iter()
                    .find(|other| &other.site_id == source)
                    .ok_or_else(|| format!("MirrorOf({source}): no layout has that site id"))?;
                if matches!(source_layout.plan, AuthoredPlan::MirrorOf(_)) {
                    return Err(format!(
                        "MirrorOf({source}): that layout is a mirror itself"
                    ));
                }
                self.plan_parts(source_layout)
            },
        }
    }

    /// Every layout, resolved into world space, keyed by settlement id.
    /// `extra_exclusions` (the authored landmarks' footprints) apply to
    /// every layout. Call after [`Self::validate`].
    pub(super) fn resolve(
        &self,
        pins: &AuthoredPins,
        extra_exclusions: &[LayoutExclusion],
    ) -> HashMap<String, SettlementLayout> {
        self.layouts
            .iter()
            .map(|layout| {
                let resolved = self
                    .resolve_one(layout, pins, extra_exclusions)
                    .unwrap_or_else(|err| {
                        panic!(
                            "validated settlement layout {} failed: {err}",
                            layout.site_id
                        )
                    });
                (layout.site_id.clone(), resolved)
            })
            .collect()
    }

    fn resolve_one(
        &self,
        layout: &AuthoredSettlementLayout,
        pins: &AuthoredPins,
        extra_exclusions: &[LayoutExclusion],
    ) -> Result<SettlementLayout, String> {
        let (pin, target) = *pins
            .settlements
            .get(&layout.site_id)
            .ok_or_else(|| "no authored settlement has this id".to_string())?;
        let origin = match layout.frame.origin {
            AuthoredFrameOrigin::Pin => pin,
            AuthoredFrameOrigin::Wpos(x, y) => Vec2::new(x, y),
        };
        let water_side = match layout.frame.water_side {
            AuthoredWaterSide::Left => WaterSide::Left,
            AuthoredWaterSide::Right => WaterSide::Right,
        };
        let frame = LayoutFrame::new(origin.as_(), layout.frame.along_deg, water_side);
        let parts = self.plan_parts(layout)?;
        let wards = match parts.wards {
            WardsSource::Wards(wards) => wards
                .iter()
                .map(|ward| LayoutWard {
                    id: ward.id.clone(),
                    a: ward.a,
                    b: ward.b,
                    target_buildings: ward.target_buildings,
                })
                .collect(),
            WardsSource::Sectors(sectors) => {
                // `a` grows toward the water side; left/right are relative
                // to the bank direction.
                let a = match water_side {
                    WaterSide::Right => (-sectors.left, sectors.right),
                    WaterSide::Left => (-sectors.right, sectors.left),
                };
                vec![LayoutWard {
                    id: "sectors".to_string(),
                    a,
                    b: (-sectors.back, sectors.front),
                    target_buildings: target,
                }]
            },
        };
        let exclusions = layout
            .exclude_sites
            .iter()
            .map(|exclusion| {
                pins.pin_of(&exclusion.site_id)
                    .map(|centre_wpos| LayoutExclusion {
                        centre_wpos,
                        radius: exclusion.radius_blocks,
                    })
                    .ok_or_else(|| format!("unknown exclude_sites id {}", exclusion.site_id))
            })
            .chain(extra_exclusions.iter().copied().map(Ok))
            .collect::<Result<_, _>>()?;
        Ok(SettlementLayout {
            frame,
            anchor: Vec2::new(parts.anchor.a, parts.anchor.b),
            plaza_radius: parts.plaza_radius,
            fields: match parts.fields {
                AuthoredFields::Inside => FieldPlacement::Inside,
                AuthoredFields::Ring(width) => FieldPlacement::Ring(width),
            },
            wards,
            exclusions,
        })
    }

    pub(super) fn site_ids(&self) -> impl Iterator<Item = &str> {
        self.layouts.iter().map(|layout| layout.site_id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins() -> AuthoredPins {
        AuthoredPins {
            settlements: [
                ("site.a".to_string(), (Vec2::new(1000, 1000), 30)),
                ("site.b".to_string(), (Vec2::new(1000, 2000), 30)),
                ("site.c".to_string(), (Vec2::new(5000, 5000), 12)),
            ]
            .into_iter()
            .collect(),
            landmarks: [("site.tower".to_string(), Vec2::new(1200, 1000))]
                .into_iter()
                .collect(),
        }
    }

    const VALID: &str = r#"(
        schema: "xindeler.settlement_layouts.v1",
        layouts: [
            (
                site_id: "site.a",
                frame: (origin: Pin, along_deg: 0.0, water_side: Right),
                plan: Wards((
                    anchor: (a: -30.0, b: 30.0),
                    plaza_radius: Some(2),
                    fields: Ring(3),
                    wards: [
                        (id: "core", a: (-100.0, 0.0), b: (0.0, 100.0), target_buildings: 20),
                        (id: "edge", a: (-100.0, 0.0), b: (100.0, 200.0), target_buildings: 10),
                    ],
                )),
                exclude_sites: [(site_id: "site.tower", radius_blocks: 40)],
            ),
            (
                site_id: "site.b",
                frame: (origin: Wpos(1000, 1950), along_deg: 0.0, water_side: Left),
                plan: MirrorOf("site.a"),
            ),
            (
                site_id: "site.c",
                frame: (origin: Pin, along_deg: 90.0, water_side: Left),
                plan: Sectors((
                    anchor: (a: 0.0, b: 10.0),
                    fields: Inside,
                    front: 200.0, back: 0.0, left: 50.0, right: 100.0,
                )),
            ),
        ],
    )"#;

    fn parse(text: &str) -> AuthoredSettlementLayouts {
        AuthoredSettlementLayouts::from_bytes(Cow::Borrowed(text.as_bytes())).expect("parses")
    }

    fn validation_error(text: &str) -> String {
        parse(text)
            .validate(&pins())
            .expect_err("must not validate")
    }

    #[test]
    fn a_valid_file_resolves_with_mirrors_sectors_and_exclusions() {
        let layouts = parse(VALID);
        layouts.validate(&pins()).unwrap();
        let landmark = LayoutExclusion {
            centre_wpos: Vec2::new(9, 9),
            radius: 5,
        };
        let resolved = layouts.resolve(&pins(), &[landmark]);
        let a = &resolved["site.a"];
        assert_eq!(a.target_buildings(), 30);
        assert_eq!(a.fields, FieldPlacement::Ring(3));
        assert_eq!(a.exclusions, vec![
            LayoutExclusion {
                centre_wpos: Vec2::new(1200, 1000),
                radius: 40
            },
            landmark
        ]);
        // Along east, water right (south): the anchor is 30 blocks north.
        assert!(a.anchor_wpos().distance(Vec2::new(1030.0, 1030.0)) < 1e-9);
        // The mirror: same plan, its own frame, water on the other side.
        let b = &resolved["site.b"];
        assert_eq!(b.wards, a.wards);
        assert_eq!(b.plaza_radius, Some(2));
        assert!(b.anchor_wpos().distance(Vec2::new(1030.0, 1920.0)) < 1e-9);
        // Sectors: one ward with the settlement's own target.
        let c = &resolved["site.c"];
        assert_eq!(c.wards.len(), 1);
        assert_eq!(c.target_buildings(), 12);
        assert_eq!(c.wards[0].a, (-100.0, 50.0));
        assert_eq!(c.wards[0].b, (0.0, 200.0));
    }

    #[test]
    fn bad_layouts_are_rejected_with_a_reason() {
        let cases = [
            (
                "site.a\",\n                frame",
                "site.zz\",\n                frame",
                "no authored settlement",
            ),
            (
                "target_buildings: 10",
                "target_buildings: 11",
                "add up to 31",
            ),
            ("b: (100.0, 200.0)", "b: (50.0, 200.0)", "overlap"),
            (
                "b: (100.0, 200.0)",
                "b: (200.0, 100.0)",
                "empty or non-finite",
            ),
            (
                "anchor: (a: -30.0, b: 30.0)",
                "anchor: (a: 30.0, b: 30.0)",
                "in no ward",
            ),
            (
                "plaza_radius: Some(2)",
                "plaza_radius: Some(4)",
                "plaza_radius",
            ),
            ("fields: Ring(3)", "fields: Ring(0)", "Ring(0)"),
            ("radius_blocks: 40", "radius_blocks: 0", "radius of 0"),
            (
                "site_id: \"site.tower\"",
                "site_id: \"site.nowhere\"",
                "site.nowhere",
            ),
            (
                "MirrorOf(\"site.a\")",
                "MirrorOf(\"site.c2\")",
                "MirrorOf(site.c2)",
            ),
            ("id: \"edge\"", "id: \"core\"", "duplicate"),
            ("front: 200.0", "front: -1.0", "sector front"),
            (
                "frame: (origin: Pin, along_deg: 0.0",
                "frame: (origin: Wpos(99999, 1000), along_deg: 0.0",
                "tiles from the settlement's pin",
            ),
        ];
        for (from, to, expected) in cases {
            assert!(VALID.contains(from), "{from}");
            let error = validation_error(&VALID.replacen(from, to, 1));
            assert!(error.contains(expected), "{to}: {error}");
        }
        let unknown_field = VALID.replacen("fields: Inside,", "fields: Inside, walls: true,", 1);
        assert!(
            AuthoredSettlementLayouts::from_bytes(Cow::Borrowed(unknown_field.as_bytes())).is_err(),
            "an unknown field is a parse error"
        );
        let doubled = VALID.replacen("site_id: \"site.c\"", "site_id: \"site.a\"", 1);
        assert!(validation_error(&doubled).contains("more than one layout"));
    }

    #[test]
    fn a_mirror_of_a_mirror_is_rejected() {
        let text = VALID.replacen(
            "plan: Sectors((\n                    anchor: (a: 0.0, b: 10.0),\n                    \
             fields: Inside,\n                    front: 200.0, back: 0.0, left: 50.0, right: \
             100.0,\n                )),",
            "plan: MirrorOf(\"site.b\"),",
            1,
        );
        assert!(text.contains("MirrorOf(\"site.b\")"));
        assert!(validation_error(&text).contains("is a mirror itself"));
    }
}
