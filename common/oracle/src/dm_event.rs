//! `DmEvent` — the ORACLE "drop a file, spin up an encounter" schema.
//!
//! Ported from `bevy/xindeler-oracle-host/src/dm_event.rs`. This crate
//! carries only the pure data schema and its anti-chaos sanitization — no
//! `AssetLoader`/`Plugin`/engine-ECS glue, since untrusted `.dmevent.ron`/
//! `.dmevent.json` files must sanitize identically regardless of which
//! runtime (filesystem watcher, test harness) reads them. The filesystem
//! watcher that actually feeds these into the server lives in
//! `server/src/oracle/watcher.rs`.
//!
//! `DmEvent.atmosphere` does NOT reuse Bevy's `AtmosphereProfile` (renderer
//! types this engine doesn't have — fog, sky ambient, sun illuminance). It
//! uses [`PlanoAtmosphere`], a reduced schema carrying only the fields that
//! map onto primitives this engine already has: `time_lock`
//! (`common::resources::TimeOfDay`, hours-of-day), `weather_effect` (mirrors
//! `common::weather::WeatherKind`'s variant set), and `transition_secs`.
//! Applying those to the live sim is not implemented here — this crate only
//! loads and validates the values.

/// Truncates `s` to at most `max_bytes` bytes, walking back to the nearest
/// char boundary so a hostile string that splits a multi-byte char exactly at
/// `max_bytes` can never panic.
pub(crate) fn truncate_to(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Clamps `value` into `bounds` if finite, otherwise falls back to `default`
/// — the shared anti-chaos primitive every numeric field in this module
/// sanitizes through (NaN/inf are never clamp-able, so they must be replaced
/// outright).
pub(crate) fn sane(value: f32, bounds: (f32, f32), default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(bounds.0, bounds.1)
    } else {
        default
    }
}

/// Clamp bounds enforced by [`DmEvent::sanitize`] (anti-chaos: `DmEvent`
/// files are a surface ORACLE's LLM-side tooling writes, so hostile/buggy
/// values are the threat model).
pub mod bounds {
    /// `SpawningRules::spawn_count` (the entity factory rounds this down to a
    /// whole number before actually spawning).
    pub const SPAWN_COUNT: (f32, f32) = (0.0, 200.0);
    /// `SpawningRules::spawn_radius`, blocks.
    pub const SPAWN_RADIUS: (f32, f32) = (0.0, 400.0);
    /// Ceiling on any free-form string field (`biome_profile`, narrative
    /// text, entity template ids, ...) — a defensive floor against an
    /// unboundedly large hostile value, not a game-design number.
    pub const MAX_STRING_LEN: usize = 4096;
    /// Ceiling on `SpawningRules::entity_templates`'s length.
    pub const MAX_ENTITY_TEMPLATES: usize = 64;
    /// `ai_behavior_override` allowlist: presets over the existing
    /// `server-agent` `Agent` (stalk/aggro/flee) plus the safe default. An
    /// unknown string clamps to [`DEFAULT_AI_BEHAVIOR`] rather than being
    /// rejected ("defuse, don't crash").
    pub const KNOWN_AI_BEHAVIORS: &[&str] = &["passive", "stalk", "aggro", "flee"];
    /// Safe fallback preset for an unrecognized `ai_behavior_override`.
    pub const DEFAULT_AI_BEHAVIOR: &str = "passive";
    /// `PlanoAtmosphere::transition_secs`, seconds.
    pub const TRANSITION_SECS: (f32, f32) = (0.0, 3600.0);
}

/// Mirrors `common::weather::WeatherKind`'s variant set (Clear/Cloudy/Rain/
/// Storm) so a later conversion into that engine type is a straight 1:1
/// match. Kept as a local copy rather than depending on the engine crate for
/// a single enum — this crate has no engine/specs deps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WeatherEffect {
    #[default]
    Clear,
    Cloudy,
    Rain,
    Storm,
}

/// Reduced, voxygen-native replacement for Bevy's `AtmosphereProfile`: only
/// the three fields that map onto primitives this engine already has. Ships
/// data-only — nothing in this crate applies `time_lock`/`weather_effect` to
/// the live sim.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PlanoAtmosphere {
    /// Hour-of-day lock, 0.0..24.0 on a wheel (wraps via `rem_euclid`).
    /// `None` means "follow the world's normal day/night cycle".
    pub time_lock: Option<f32>,
    /// Categorical weather override for the plano.
    pub weather_effect: WeatherEffect,
    /// How long a transition into this atmosphere takes, seconds.
    pub transition_secs: f32,
}

impl Default for PlanoAtmosphere {
    fn default() -> Self {
        Self {
            time_lock: None,
            weather_effect: WeatherEffect::default(),
            transition_secs: 5.0,
        }
    }
}

impl PlanoAtmosphere {
    fn sanitize(&mut self) {
        let defaults = Self::default();
        self.time_lock = self
            .time_lock
            .and_then(|hour| hour.is_finite().then(|| hour.rem_euclid(24.0)));
        self.transition_secs = sane(
            self.transition_secs,
            bounds::TRANSITION_SECS,
            defaults.transition_secs,
        );
    }
}

/// One dungeon-master event: a self-contained "spin up an instanced
/// encounter" spec ORACLE's tooling writes as a file. `#[serde(default)]`
/// throughout so partial files keep loading as the schema grows.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DmEvent {
    /// World-gen parameters for the plano this event spins up.
    pub dimension_config: DimensionConfig,
    /// Atmosphere override for the plano.
    pub atmosphere: PlanoAtmosphere,
    /// What (and how many) monsters populate the instance.
    pub spawning_rules: SpawningRules,
    /// DM-flavour text hooks.
    pub narrative: Narrative,
}

impl DmEvent {
    /// Anti-chaos clamps: runs on every ingestion path so no unclamped value
    /// can ever reach a later consumer (factory/narrative/lifecycle).
    /// Idempotent.
    pub fn sanitize(&mut self) {
        self.atmosphere.sanitize();
        self.dimension_config.sanitize();
        self.spawning_rules.sanitize();
        self.narrative.sanitize();
    }
}

/// World-gen parameters for the plano a [`DmEvent`] spins up.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DimensionConfig {
    /// Procgen seed offset. Any `u64` is a legal seed — the type itself
    /// already excludes NaN/inf, and no value is "unsafe" — so, unlike every
    /// float field in this module, this one is deliberately exempt from a
    /// `bounds::` clamp.
    pub seed_modifier: u64,
    /// Named biome profile injected into the generator. Free-form for now —
    /// no allowlist exists yet to validate against — so `sanitize` only
    /// enforces a defensive length cap, not a content allowlist.
    pub biome_profile: String,
}

impl Default for DimensionConfig {
    fn default() -> Self {
        Self {
            seed_modifier: 0,
            biome_profile: "default".to_owned(),
        }
    }
}

impl DimensionConfig {
    fn sanitize(&mut self) { truncate_to(&mut self.biome_profile, bounds::MAX_STRING_LEN); }
}

/// What (and how many) monsters a [`DmEvent`] spawns.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SpawningRules {
    /// `EntityTemplate` ids to draw spawns from.
    pub entity_templates: Vec<String>,
    /// How many entities to spawn. A plain `f32` (not `u32`/`usize`) so a
    /// hostile float value (NaN/inf/huge) is representable in a crafted file
    /// and exercised by [`Self::sanitize`] exactly like every other numeric
    /// field in this module — the entity factory rounds this down to a whole
    /// count when it actually spawns.
    pub spawn_count: f32,
    /// Spawn scatter radius, blocks.
    pub spawn_radius: f32,
    /// Resolves through a behavior registry (stalk/aggro/flee presets over
    /// the existing `server-agent` `Agent`). An unknown string clamps to
    /// [`bounds::DEFAULT_AI_BEHAVIOR`] rather than being rejected.
    pub ai_behavior_override: String,
}

impl Default for SpawningRules {
    fn default() -> Self {
        Self {
            entity_templates: Vec::new(),
            spawn_count: 0.0,
            spawn_radius: 50.0,
            ai_behavior_override: bounds::DEFAULT_AI_BEHAVIOR.to_owned(),
        }
    }
}

impl SpawningRules {
    fn sanitize(&mut self) {
        let defaults = Self::default();
        self.spawn_count = sane(self.spawn_count, bounds::SPAWN_COUNT, defaults.spawn_count);
        self.spawn_radius = sane(
            self.spawn_radius,
            bounds::SPAWN_RADIUS,
            defaults.spawn_radius,
        );
        if !bounds::KNOWN_AI_BEHAVIORS.contains(&self.ai_behavior_override.as_str()) {
            self.ai_behavior_override = bounds::DEFAULT_AI_BEHAVIOR.to_owned();
        }
        if self.entity_templates.len() > bounds::MAX_ENTITY_TEMPLATES {
            self.entity_templates.truncate(bounds::MAX_ENTITY_TEMPLATES);
        }
        for template in &mut self.entity_templates {
            truncate_to(template, bounds::MAX_STRING_LEN);
        }
    }
}

/// DM-flavour text hooks (the narrative/chronicle hooks consume these —
/// `world_rumor` into the chronicle-hook log, `on_enter_message` into a HUD
/// toast).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Narrative {
    /// Appended to the chronicle-hook log the moment the event loads.
    pub world_rumor: Option<String>,
    /// Sent to the one client whose character enters this event's plano.
    pub on_enter_message: Option<String>,
}

impl Narrative {
    fn sanitize(&mut self) {
        if let Some(text) = &mut self.world_rumor {
            truncate_to(text, bounds::MAX_STRING_LEN);
        }
        if let Some(text) = &mut self.on_enter_message {
            truncate_to(text, bounds::MAX_STRING_LEN);
        }
    }
}

/// Parses `bytes` as RON (`is_json == false`) or JSON, then sanitizes. A free
/// function (not inlined into the filesystem watcher) so tests can exercise
/// malformed input directly without spinning up any runtime.
pub fn parse_dm_event(bytes: &[u8], is_json: bool) -> Result<DmEvent, ParseError> {
    let mut event: DmEvent = if is_json {
        serde_json::from_slice(bytes)?
    } else {
        ron::de::from_bytes(bytes)?
    };
    // Anti-chaos: ORACLE-written files are untrusted input.
    event.sanitize();
    Ok(event)
}

/// Parse failure for a `.dmevent.ron` / `.dmevent.json` file — never a panic,
/// always a `Result` the caller (the filesystem watcher) can log and ignore.
#[derive(Debug)]
pub enum ParseError {
    Ron(ron::de::SpannedError),
    Json(serde_json::Error),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ron(err) => write!(f, "invalid RON: {err}"),
            Self::Json(err) => write!(f, "invalid JSON: {err}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<ron::de::SpannedError> for ParseError {
    fn from(err: ron::de::SpannedError) -> Self { Self::Ron(err) }
}

impl From<serde_json::Error> for ParseError {
    fn from(err: serde_json::Error) -> Self { Self::Json(err) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hostile_dm_event() -> DmEvent {
        DmEvent {
            dimension_config: DimensionConfig {
                seed_modifier: u64::MAX,
                biome_profile: "x".repeat(bounds::MAX_STRING_LEN * 4),
            },
            atmosphere: PlanoAtmosphere {
                time_lock: Some(37.0),
                weather_effect: WeatherEffect::Storm,
                transition_secs: -1.0,
            },
            spawning_rules: SpawningRules {
                entity_templates: (0..bounds::MAX_ENTITY_TEMPLATES * 4)
                    .map(|i| "t".repeat(bounds::MAX_STRING_LEN * 2) + &i.to_string())
                    .collect(),
                spawn_count: f32::NAN,
                spawn_radius: -500.0,
                ai_behavior_override: "definitely_not_a_real_behavior".to_owned(),
            },
            narrative: Narrative {
                world_rumor: Some("y".repeat(bounds::MAX_STRING_LEN * 4)),
                on_enter_message: None,
            },
        }
    }

    #[test]
    fn sanitize_defuses_hostile_dm_events() {
        let mut garbage = hostile_dm_event();
        garbage.sanitize();

        assert_eq!(garbage.atmosphere.time_lock, Some(13.0)); // 37 h -> 13 h on the wheel
        assert!((garbage.atmosphere.transition_secs - 0.0).abs() < f32::EPSILON);

        assert!(garbage.dimension_config.biome_profile.len() <= bounds::MAX_STRING_LEN);

        assert!(garbage.spawning_rules.spawn_count.is_finite());
        assert!(
            (bounds::SPAWN_COUNT.0..=bounds::SPAWN_COUNT.1)
                .contains(&garbage.spawning_rules.spawn_count)
        );
        assert!(
            (bounds::SPAWN_RADIUS.0..=bounds::SPAWN_RADIUS.1)
                .contains(&garbage.spawning_rules.spawn_radius)
        );
        assert_eq!(
            garbage.spawning_rules.ai_behavior_override,
            bounds::DEFAULT_AI_BEHAVIOR
        );
        assert!(garbage.spawning_rules.entity_templates.len() <= bounds::MAX_ENTITY_TEMPLATES);
        for template in &garbage.spawning_rules.entity_templates {
            assert!(template.len() <= bounds::MAX_STRING_LEN);
        }

        assert!(
            garbage.narrative.world_rumor.expect("still present").len() <= bounds::MAX_STRING_LEN
        );
        assert_eq!(garbage.narrative.on_enter_message, None);

        // A non-hostile event is untouched by sanitize.
        let mut sane_event = DmEvent::default();
        sane_event.sanitize();
        assert_eq!(sane_event, DmEvent::default());
    }

    #[test]
    fn sanitize_is_idempotent() {
        let mut event = hostile_dm_event();
        event.sanitize();
        let once = event.clone();
        event.sanitize();
        assert_eq!(event, once);
    }

    #[test]
    fn time_lock_nan_clears_to_none() {
        let mut atmosphere = PlanoAtmosphere {
            time_lock: Some(f32::NAN),
            ..Default::default()
        };
        atmosphere.sanitize();
        assert_eq!(atmosphere.time_lock, None);
    }

    #[test]
    fn both_extensions_parse_identical_content_to_equal_values() {
        let original = DmEvent {
            dimension_config: DimensionConfig {
                seed_modifier: 42,
                biome_profile: "mist_bound_mist".to_owned(),
            },
            atmosphere: PlanoAtmosphere {
                time_lock: Some(23.5),
                weather_effect: WeatherEffect::Rain,
                transition_secs: 5.0,
            },
            spawning_rules: SpawningRules {
                entity_templates: vec!["ghost_wolf".to_owned(), "banshee".to_owned()],
                spawn_count: 6.0,
                spawn_radius: 120.0,
                ai_behavior_override: "aggro".to_owned(),
            },
            narrative: Narrative {
                world_rumor: Some("A cold mist swallows the village.".to_owned()),
                on_enter_message: Some("The gate to the Mist-Bound creaks open.".to_owned()),
            },
        };

        let ron_text = ron::ser::to_string(&original).expect("DmEvent serializes to RON");
        let json_text = serde_json::to_string(&original).expect("DmEvent serializes to JSON");

        let from_ron = parse_dm_event(ron_text.as_bytes(), false).expect(".dmevent.ron parses");
        let from_json = parse_dm_event(json_text.as_bytes(), true).expect(".dmevent.json parses");

        // `original`'s values are already sane, so `parse_dm_event`'s
        // sanitize pass is a no-op and both extensions must agree exactly.
        assert_eq!(from_ron, original);
        assert_eq!(from_json, original);
    }

    #[test]
    fn malformed_input_fails_without_panic() {
        assert!(
            parse_dm_event(b"not valid ron {{{", false).is_err(),
            "garbage RON must fail the load, not panic"
        );
        assert!(
            parse_dm_event(b"{not valid json", true).is_err(),
            "garbage JSON must fail the load, not panic"
        );
    }

    /// The shipped `mist_bound.dmevent.ron` sample (the file a human drops
    /// into the watched events directory to trigger `/oracle_trigger
    /// mist_bound`) parses AND `sanitize()` is a no-op — every value in it
    /// must already sit inside `bounds`.
    #[test]
    fn shipped_mist_bound_dmevent_parses_and_is_already_sane() {
        let text = include_str!("../../../assets/common/oracle/events/mist_bound.dmevent.ron");
        let mut parsed: DmEvent = ron::from_str(text).expect("mist_bound.dmevent.ron parses");

        assert_eq!(parsed.dimension_config.seed_modifier, 1_298_754_643);
        assert_eq!(
            parsed.dimension_config.biome_profile,
            "mist_bound_grey_forest"
        );
        assert_eq!(parsed.atmosphere.time_lock, Some(23.5));
        assert_eq!(parsed.atmosphere.weather_effect, WeatherEffect::Rain);
        assert_eq!(parsed.spawning_rules.entity_templates, vec![
            "mist_bound_shade".to_owned()
        ]);
        assert!((parsed.spawning_rules.spawn_count - 15.0).abs() < f32::EPSILON);
        assert_eq!(parsed.spawning_rules.ai_behavior_override, "aggro");
        assert!(parsed.narrative.world_rumor.is_some());
        assert!(parsed.narrative.on_enter_message.is_some());

        let before = parsed.clone();
        parsed.sanitize();
        assert_eq!(
            parsed, before,
            "mist_bound.dmevent.ron should already be sane (sanitize must be a no-op)"
        );
    }
}
