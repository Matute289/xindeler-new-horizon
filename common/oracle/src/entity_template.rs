//! `EntityTemplate` — the generic, data-driven entity factory schema.
//!
//! Ported from `bevy/xindeler-oracle-host/src/entity_template.rs`. Only the
//! pure schema + anti-chaos sanitization live here, mirroring
//! [`crate::dm_event`]'s split: the actual spawn logic (turning a template
//! into real ECS components in the live `specs::World`) lives in
//! `server/src/oracle/factory.rs`, which depends on the engine crate this one
//! deliberately does not.
//!
//! Bevy's original also carried `PendingBody`/`PendingStats`/… descriptor
//! components and a `spawn_entity_template` function that attached them to a
//! transient entity for a second crate to pick up later. That indirection
//! existed only to cross Bevy's two-crate isolation boundary (the loader
//! crate never touches a live `World`). This engine has no such boundary — a
//! server-side consumer can spawn directly into `specs::World` from an
//! `EntityTemplate`'s fields, so those descriptor types are not ported.

use crate::dm_event::bounds as dm_bounds;

/// One entity-factory template: `entity_template_id` names it; the other
/// fields are the fixed set of "component kinds" a factory turns into real
/// spawn data. `#[serde(default)]` throughout so partial files keep loading
/// as the schema grows, exactly like [`crate::dm_event::DmEvent`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EntityTemplate {
    /// Human-readable id for logging/debugging; not used for dispatch.
    pub entity_template_id: String,
    /// An existing NPC body keyword (e.g. `"wolf"`, `"pig"`, `"humanoid"`) —
    /// the same string vocabulary the in-game `/spawn` admin command already
    /// parses. Never a new, parallel body-naming scheme.
    pub body: String,
    /// Minimal display stats (v1 deliberately does not expose
    /// balance-affecting numeric fields here — that is game-content tuning,
    /// out of this schema's scope).
    pub stats: EntityTemplateStats,
    /// One of [`bounds::KNOWN_FACTIONS`] (the spawnable alignment variants,
    /// lowercased); unknown falls back to [`bounds::DEFAULT_FACTION`].
    pub faction: String,
    /// An existing item asset specifier, or `None` for no loot.
    pub loot: Option<String>,
    /// Resolves through [`AgentPreset::resolve`] — one of
    /// [`crate::dm_event::bounds::KNOWN_AI_BEHAVIORS`]; unknown/malformed
    /// falls back to [`AgentPreset::Passive`] rather than panicking.
    pub ai_behavior_override: String,
}

impl Default for EntityTemplate {
    fn default() -> Self {
        Self {
            entity_template_id: String::new(),
            body: "pig".to_owned(),
            stats: EntityTemplateStats::default(),
            faction: bounds::DEFAULT_FACTION.to_owned(),
            loot: None,
            ai_behavior_override: dm_bounds::DEFAULT_AI_BEHAVIOR.to_owned(),
        }
    }
}

/// Minimal display stats a template can specify (the `"stats"` component
/// kind). Deliberately thin — see [`EntityTemplate::stats`]'s doc comment.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EntityTemplateStats {
    /// Display name; `None` lets the spawn fall back to the body's own
    /// default name.
    pub name: Option<String>,
}

/// Clamp bounds enforced by [`EntityTemplate::sanitize`] (anti-chaos, same
/// threat model as [`crate::dm_event::bounds`]: template files are content
/// authored by ORACLE's LLM-side tooling as well as by hand, so hostile/buggy
/// values are in scope). Numeric-string-length bounds are reused directly
/// from `dm_event::bounds` (`MAX_STRING_LEN`) rather than re-declared here.
pub mod bounds {
    /// `EntityTemplate::faction` allowlist — the spawnable alignment
    /// variants, lowercased (an owned/tamed-by-uid alignment is excluded: it
    /// needs a runtime owner entity, not expressible in a static template).
    pub const KNOWN_FACTIONS: &[&str] = &["wild", "enemy", "npc", "tame", "passive"];
    /// Safe fallback for an unrecognized `faction` string.
    pub const DEFAULT_FACTION: &str = "wild";
}

impl EntityTemplate {
    /// Anti-chaos clamps (mirrors [`crate::dm_event::DmEvent::sanitize`]'s
    /// shape exactly): truncates every free-form string, and validates
    /// `faction`/`ai_behavior_override` against their closed allowlists,
    /// falling back to a safe default rather than ever propagating a hostile
    /// value downstream. Idempotent. Runs on every ingestion path (today:
    /// [`parse_entity_template`]).
    pub fn sanitize(&mut self) {
        crate::dm_event::truncate_to(&mut self.entity_template_id, dm_bounds::MAX_STRING_LEN);
        crate::dm_event::truncate_to(&mut self.body, dm_bounds::MAX_STRING_LEN);
        if let Some(name) = &mut self.stats.name {
            crate::dm_event::truncate_to(name, dm_bounds::MAX_STRING_LEN);
        }
        if !bounds::KNOWN_FACTIONS.contains(&self.faction.as_str()) {
            self.faction = bounds::DEFAULT_FACTION.to_owned();
        }
        if let Some(loot) = &mut self.loot {
            crate::dm_event::truncate_to(loot, dm_bounds::MAX_STRING_LEN);
        }
        if !dm_bounds::KNOWN_AI_BEHAVIORS.contains(&self.ai_behavior_override.as_str()) {
            self.ai_behavior_override = dm_bounds::DEFAULT_AI_BEHAVIOR.to_owned();
        }
    }
}

/// A named preset over the engine's existing `Agent`/`Psyche` knobs. Every
/// variant just tunes the same fields the `/spawn`/`/make_npc` admin commands
/// already tune by hand — deliberately not a new behavior-tree interpreter.
/// Resolution logic only; the actual `Agent`/`Psyche` construction (which
/// needs the engine crate) lives in `server/src/oracle/factory.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentPreset {
    /// Never notices anyone and never flees. The safe anti-chaos fallback
    /// for an unknown/malformed `ai_behavior_override` string — matches
    /// [`crate::dm_event::bounds::DEFAULT_AI_BEHAVIOR`] (`"passive"`).
    #[default]
    Passive,
    /// The body's own default wander/notice/engage behavior, untouched.
    Stalk,
    /// Notices from farther away, skips the aggro warn-up, never flees.
    Aggro,
    /// Always flees rather than fighting.
    Flee,
}

impl AgentPreset {
    /// Resolves an `ai_behavior_override` string to a preset, falling back
    /// to [`AgentPreset::Passive`] for anything outside
    /// [`crate::dm_event::bounds::KNOWN_AI_BEHAVIORS`] — the same closed set
    /// `DmEvent::sanitize`/`EntityTemplate::sanitize` already validate
    /// against, so a string that survived either sanitize pass always
    /// resolves to a real variant here too.
    #[must_use]
    pub fn resolve(name: &str) -> Self {
        match name {
            "stalk" => Self::Stalk,
            "aggro" => Self::Aggro,
            "flee" => Self::Flee,
            // "passive" and any unrecognized string both land here — the
            // fallback and the explicit choice are the same safe preset.
            _ => Self::Passive,
        }
    }
}

/// Parses `bytes` as RON (`is_json == false`) or JSON, then sanitizes. A free
/// function so tests can exercise malformed input directly.
pub fn parse_entity_template(
    bytes: &[u8],
    is_json: bool,
) -> Result<EntityTemplate, crate::dm_event::ParseError> {
    let mut template: EntityTemplate = if is_json {
        serde_json::from_slice(bytes)?
    } else {
        ron::de::from_bytes(bytes)?
    };
    template.sanitize();
    Ok(template)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hostile_template() -> EntityTemplate {
        EntityTemplate {
            entity_template_id: "x".repeat(dm_bounds::MAX_STRING_LEN * 4),
            body: "y".repeat(dm_bounds::MAX_STRING_LEN * 4),
            stats: EntityTemplateStats {
                name: Some("z".repeat(dm_bounds::MAX_STRING_LEN * 4)),
            },
            faction: "definitely_not_a_real_faction".to_owned(),
            loot: Some("w".repeat(dm_bounds::MAX_STRING_LEN * 4)),
            ai_behavior_override: "definitely_not_a_real_behavior".to_owned(),
        }
    }

    #[test]
    fn sanitize_defuses_hostile_templates() {
        let mut garbage = hostile_template();
        garbage.sanitize();

        assert!(garbage.entity_template_id.len() <= dm_bounds::MAX_STRING_LEN);
        assert!(garbage.body.len() <= dm_bounds::MAX_STRING_LEN);
        assert!(
            garbage.stats.name.as_ref().expect("still present").len() <= dm_bounds::MAX_STRING_LEN
        );
        assert_eq!(garbage.faction, bounds::DEFAULT_FACTION);
        assert!(garbage.loot.as_ref().expect("still present").len() <= dm_bounds::MAX_STRING_LEN);
        assert_eq!(garbage.ai_behavior_override, dm_bounds::DEFAULT_AI_BEHAVIOR);

        // A non-hostile template is untouched by sanitize.
        let mut sane = EntityTemplate::default();
        sane.sanitize();
        assert_eq!(sane, EntityTemplate::default());
    }

    #[test]
    fn sanitize_is_idempotent() {
        let mut template = hostile_template();
        template.sanitize();
        let once = template.clone();
        template.sanitize();
        assert_eq!(template, once);
    }

    #[test]
    fn both_extensions_parse_identical_content_to_equal_values() {
        let original = EntityTemplate {
            entity_template_id: "dread_wolf".to_owned(),
            body: "wolf".to_owned(),
            stats: EntityTemplateStats {
                name: Some("Dread Wolf".to_owned()),
            },
            faction: "enemy".to_owned(),
            loot: Some("common.items.crafting_ing.hide.tough".to_owned()),
            ai_behavior_override: "aggro".to_owned(),
        };

        let ron_text = ron::ser::to_string(&original).expect("EntityTemplate serializes to RON");
        let json_text =
            serde_json::to_string(&original).expect("EntityTemplate serializes to JSON");

        let from_ron =
            parse_entity_template(ron_text.as_bytes(), false).expect(".entity_template.ron parses");
        let from_json = parse_entity_template(json_text.as_bytes(), true)
            .expect(".entity_template.json parses");

        assert_eq!(from_ron, original);
        assert_eq!(from_json, original);
    }

    #[test]
    fn malformed_input_fails_without_panic() {
        assert!(
            parse_entity_template(b"not valid ron {{{", false).is_err(),
            "garbage RON must fail the load, not panic"
        );
        assert!(
            parse_entity_template(b"{not valid json", true).is_err(),
            "garbage JSON must fail the load, not panic"
        );
    }

    /// The four shipped sample templates (`assets/common/oracle/
    /// entity_templates/*.entity_template.ron`) parse, are already sane
    /// (`sanitize` is a no-op), and each use a different
    /// `ai_behavior_override` preset — a concrete demonstration that
    /// authoring a new template asset requires zero Rust changes.
    #[test]
    fn shipped_sample_templates_parse_and_are_already_sane() {
        let fixtures = [
            (
                "dread_wolf",
                include_str!(
                    "../../../assets/common/oracle/entity_templates/dread_wolf.entity_template.ron"
                ),
                "aggro",
            ),
            (
                "forest_deer",
                include_str!(
                    "../../../assets/common/oracle/entity_templates/forest_deer.entity_template.\
                     ron"
                ),
                "flee",
            ),
            (
                "sentinel_owl",
                include_str!(
                    "../../../assets/common/oracle/entity_templates/sentinel_owl.entity_template.\
                     ron"
                ),
                "stalk",
            ),
            (
                "mist_bound_shade",
                include_str!(
                    "../../../assets/common/oracle/entity_templates/mist_bound_shade.\
                     entity_template.ron"
                ),
                "aggro",
            ),
        ];

        for (id, text, expected_behavior) in fixtures {
            let mut parsed: EntityTemplate =
                ron::from_str(text).unwrap_or_else(|e| panic!("{id} parses: {e}"));
            assert_eq!(parsed.entity_template_id, id);
            assert_eq!(parsed.ai_behavior_override, expected_behavior);

            let before = parsed.clone();
            parsed.sanitize();
            assert_eq!(
                parsed, before,
                "{id} should already be sane (sanitize must be a no-op)"
            );
        }
    }

    #[test]
    fn agent_preset_resolve_falls_back_to_passive_for_unknown() {
        assert_eq!(AgentPreset::resolve("stalk"), AgentPreset::Stalk);
        assert_eq!(AgentPreset::resolve("aggro"), AgentPreset::Aggro);
        assert_eq!(AgentPreset::resolve("flee"), AgentPreset::Flee);
        assert_eq!(AgentPreset::resolve("passive"), AgentPreset::Passive);
        assert_eq!(
            AgentPreset::resolve("definitely_not_a_real_behavior"),
            AgentPreset::Passive,
            "an unknown ai_behavior_override string must default to Passive, never panic"
        );
    }
}
