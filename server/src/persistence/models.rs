pub struct Character {
    pub character_id: i64,
    #[expect(dead_code)]
    pub player_uuid: String,
    pub alias: String,
    pub waypoint: Option<String>,
    pub hardcore: i64,
    pub class: String,
    /// `NULL` -> single-class (`CharacterClass::secondary == None`).
    pub secondary_class: Option<String>,
    /// Banked class levels in the secondary class (`0` for single-class).
    /// The primary's own level is never stored -- always derived as
    /// `character_level - secondary_class_level`.
    pub secondary_class_level: i64,
    /// Set-and-forget routing preference (`0`/`1`): whether character levels
    /// earned after the multiclass grant flow to the secondary class instead
    /// of the primary. Always `0` for single-class characters.
    pub secondary_class_future_levels: i64,
    pub ethos_good_evil: i16,
    pub ethos_law_chaos: i16,
    /// BL-31: `NULL` -> `Background(None)` ("Uncommitted", P0 §Q1).
    pub background: Option<String>,
    /// Dead column (BL-31 UI-fixes pass): `BackgroundKind::Custom` was
    /// removed, so this is never populated with real data anymore. Left in
    /// place to avoid a migration (nullable, zero live rows referenced it).
    #[expect(dead_code)]
    pub background_custom_note: Option<String>,
    /// Reactive trigger slots as a JSON array; `NULL` -> none configured.
    pub trigger_slots: Option<String>,
    /// Per-`MagicSource` mastery progress as a small JSON object; `NULL` ->
    /// nothing accrued yet (every source at 0%).
    pub spell_mastery: Option<String>,
    /// `NULL` -> `PactStanding::Bound` (only an explicit "severed" row
    /// suppresses casting).
    pub pact_standing: Option<String>,
    /// `NULL` -> no patron chosen yet (`Pact::default()`).
    pub pact_patron_id: Option<String>,
    /// Reserved for a future demand/favour mechanic; `NULL` -> `0`. Nothing
    /// reads or writes a non-zero value yet.
    pub pact_favour: Option<i32>,
}

#[derive(Debug)]
pub struct Item {
    pub item_id: i64,
    pub parent_container_item_id: i64,
    pub item_definition_id: String,
    /// `u32::MAX` must fit inside this type
    pub stack_size: i64,
    pub position: String,
    pub properties: String,
}

pub struct Body {
    #[expect(dead_code)]
    pub body_id: i64,
    pub variant: String,
    pub body_data: String,
}

pub struct SkillGroup {
    pub entity_id: i64,
    pub skill_group_kind: String,
    pub earned_exp: i64,
    pub spent_exp: i64,
    pub skills: String,
    pub hash_val: Vec<u8>,
    /// Skill points granted directly (bypassing the exp economy — e.g. a
    /// level-milestone feat point), tracked separately from `earned_exp` so
    /// they survive a save/load round-trip. See
    /// `conversions::convert_skill_groups_{from,to}_database`.
    pub direct_earned_sp: i64,
    pub direct_available_sp: i64,
}

pub struct Pet {
    pub database_id: i64,
    // TODO: add ability to store and change pet names
    //
    // Originally we just stored hardcoded English names here, but that is a bit
    // impossible now that we translate names.
    //
    // If we do implement such functionality, we probably want to use something
    // similar to current npcs that have both translated and hardcoded names
    // using `name-misc-with-alias-template`.
    // Or even some better system for displaying complex names such as this.
    #[expect(unused)]
    pub name: String,
    pub body_variant: String,
    pub body_data: String,
}

pub struct AbilitySets {
    #[expect(dead_code)]
    pub entity_id: i64,
    pub ability_sets: String,
}
