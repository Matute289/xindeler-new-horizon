//! Persistent narrative state: the durable, per-character store of *what this
//! character chose* and *how much standing they accumulated*, plus the
//! RON-authored vocabulary content reads and writes it with.
//!
//! The shape, and why it is this shape, is in the persistent-quest-state
//! design spec (private design repo). The three properties worth knowing
//! before touching anything here:
//!
//! 1. **Every value is one `i32`.** The manifest ([`NarrativeManifest`],
//!    `assets/common/narrative/variables.ron`) is what gives that integer
//!    meaning. Adding an adventure is a manifest edit plus content assets —
//!    never a new Rust type or enum variant.
//! 2. **The store is sparse.** A variable a character has never touched has no
//!    entry, and reads fall back to the manifest default. Only deviations are
//!    persisted.
//! 3. **State is keyed by character, always** (spec Tenet N-1). There is no
//!    account-scoped narrative state and there is no later phase that adds one
//!    — [`NarrativeScope`] is a closed set of three in-fiction scopes.
//!
//! Nothing here persists engine semantics: a stored row is
//! `("quest.the_kind_work.commissions", 2)`. Every interpretation lives in the
//! manifest, so a content error is fixable by editing an asset rather than by
//! shipping a save-repair script.

use crate::assets::{Asset, AssetCache, AssetExt, AssetReadGuard, BoxedError, Ron, SharedString};
use hashbrown::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use specs::{Component, DerefFlaggedStorage, HashMapStorage};
use std::borrow::Borrow;

/// The declared ceiling on how many narrative variables the manifest may hold,
/// asserted at load.
///
/// A bounded capacity that is raised **deliberately**, in a reviewed commit,
/// rather than drifting — FFXIV's `ARRSIZE_QUESTCOMPLETE` is the precedent
/// (spec §10). Ten adventures at ~20 variables each is ~200, so this is two
/// orders of headroom, not a live constraint.
pub const MAX_NARRATIVE_VARS: usize = 4096;

/// A stable, content-authored narrative variable id, e.g.
/// `"quest.the_kind_work.commissions"`.
///
/// The same dotted namespace the lore tree's own `id:` frontmatter and
/// `assets/lore/index.ron` already use — see
/// `narrative_var_prefixes_resolve_against_the_lore_index`, which is what keeps
/// this from becoming Bethesda's flat unowned global-variable namespace.
///
/// This is the **persisted** form. It is a string on purpose: `json_models`'s
/// own documented rule — *store the stable string id, never a positional
/// index* — applies verbatim, because the manifest is reorderable content.
///
/// `#[serde(transparent)]` so it is a plain string on the wire and in RON: an
/// authored id reads `"quest.the_kind_work.offer"`, not
/// `NarrativeVarId("quest.the_kind_work.offer")`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NarrativeVarId(pub String);

impl NarrativeVarId {
    pub fn new(id: impl Into<String>) -> Self { Self(id.into()) }

    pub fn as_str(&self) -> &str { &self.0 }
}

impl Borrow<str> for NarrativeVarId {
    fn borrow(&self) -> &str { &self.0 }
}

impl std::fmt::Display for NarrativeVarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
}

/// One named band of a [`NarrativeVarKind::Standing`] — the `Ethos` shape
/// (`common/src/comp/ethos.rs`) generalised and moved into data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingTier {
    pub at_least: i32,
    pub name: String,
}

/// What a narrative variable's single `i32` means.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NarrativeVarKind {
    /// Presence/absence. Stored as 1; [`NarrativeEffect::Clear`] removes the
    /// entry entirely rather than storing 0, keeping the store sparse.
    Flag,
    /// One of a fixed, ordered, named option set. Stored as the option's index
    /// *within this variable's own list*, which is why that list is
    /// **append-only** — see `choice_option_lists_are_append_only`. Reordering
    /// or deleting an option silently rewrites every character's history.
    Choice { options: Vec<String> },
    /// A plain counter, clamped to `[min, max]`.
    Tally { min: i32, max: i32, default: i32 },
    /// A bounded scalar with named threshold bands.
    Standing {
        min: i32,
        max: i32,
        default: i32,
        /// Ascending by `at_least`, lowest band at or below `min`, so every
        /// legal value has a tier. Enforced at load.
        tiers: Vec<StandingTier>,
    },
}

impl NarrativeVarKind {
    /// The value a character who has never touched this variable reads.
    ///
    /// WoW's `reputationBase` role: the default lives in the manifest, never
    /// in the save, so retuning it retunes every untouched character at once.
    pub fn default_value(&self) -> i32 {
        match self {
            NarrativeVarKind::Flag | NarrativeVarKind::Choice { .. } => 0,
            NarrativeVarKind::Tally { default, .. }
            | NarrativeVarKind::Standing { default, .. } => *default,
        }
    }

    /// The closed range a stored value is clamped into.
    fn bounds(&self) -> (i32, i32) {
        match self {
            NarrativeVarKind::Flag => (0, 1),
            NarrativeVarKind::Choice { options } => (0, options.len().saturating_sub(1) as i32),
            NarrativeVarKind::Tally { min, max, .. }
            | NarrativeVarKind::Standing { min, max, .. } => (*min, *max),
        }
    }

    /// Clamp a value into this kind's declared bounds.
    pub fn clamp(&self, value: i32) -> i32 {
        let (min, max) = self.bounds();
        value.clamp(min, max)
    }

    fn options(&self) -> Option<&[String]> {
        match self {
            NarrativeVarKind::Choice { options } => Some(options),
            _ => None,
        }
    }
}

/// Who a narrative variable's value belongs to (spec §7).
///
/// **A closed set of exactly three variants, permanently** (spec Tenet N-1).
/// Every one of them names something that exists *inside the fiction*; a
/// fourth would have to be another in-world notion (a settlement, a bloodline)
/// and may never be an out-of-game one such as an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NarrativeScope {
    /// Stored on that one character. Reputation with a faction, personal
    /// reveals, "did *you* pay the memory".
    Character,
    /// Stored on **each eligible member's own** state, written by a fan-out at
    /// the instant the choice resolves — `Group(u32)` is a runtime `Slab`
    /// index with no durable identity, so there is no party-keyed store to put
    /// it in (spec §2.5).
    Party,
    /// Stored once, globally, in rtsim's `Data::world_narrative`. A revelation
    /// made public; a structure permanently destroyed.
    World,
}

/// Whether the player may ever see a variable.
///
/// `Hidden` variables must never reach a client: they are both a spoiler
/// surface and, for option gating, an exploit surface (spec §8.5). No client
/// sync path exists yet; this is the contract the future journal inherits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NarrativeVisibility {
    Hidden,
    Journal,
}

/// One term of [`DerivedRule::SumOf`]: a predicate and what it is worth.
///
/// **Deviation from the spec's `SumOf { inputs, weights }` sketch, recorded
/// deliberately.** The sketch writes two parallel lists, but the spec's own
/// worked example needs the inputs to be *predicates*
/// (`backing != "none"`, `components_recovered == 5`), not bare ids. Pairing
/// each predicate with its own weight in one term keeps that example
/// expressible and removes the length-mismatch hazard two parallel lists
/// carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SumTerm {
    pub when: NarrativeCondition,
    pub weight: i32,
}

/// What a [`DerivedRule::ThresholdOf`] band resolves to — always an option name
/// of the derived variable itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DerivedValue {
    Const(String),
    /// Read another `Choice` variable and map its option name onto one of ours.
    FromChoice {
        input: NarrativeVarId,
        map: HashMap<String, String>,
    },
}

/// One band of [`DerivedRule::ThresholdOf`]. Bands are declared in **strictly
/// descending** `at_least` order and the first one the input reaches wins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThresholdBand {
    pub at_least: i32,
    pub then: DerivedValue,
}

/// How a derived variable is computed from its inputs.
///
/// **Derived variables are recomputed on read and never stored.** Writing one
/// directly is refused ([`EffectRefusal::Derived`]), so there is no way for a
/// stored value to disagree with its rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DerivedRule {
    /// Whichever option name the most inputs hold. Fewer than `min_inputs`
    /// *set* ⇒ `fallback`, which is how "the party did nothing" gets a named
    /// consequence rather than a special case. A tie ⇒ `tie_breaker`.
    ///
    /// Only valid on a `Choice` variable whose own option list covers every
    /// option of every input.
    MajorityOf {
        inputs: Vec<NarrativeVarId>,
        min_inputs: usize,
        fallback: String,
        tie_breaker: String,
    },
    /// The sum of every term whose predicate holds. Only valid on a `Tally` or
    /// `Standing` variable; the result is clamped to that variable's bounds.
    SumOf { inputs: Vec<SumTerm> },
    /// Band `input`'s value, highest band first. Only valid on a `Choice`
    /// variable.
    ThresholdOf {
        input: NarrativeVarId,
        bands: Vec<ThresholdBand>,
    },
}

/// One manifest entry: everything the engine knows about one narrative
/// variable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarrativeVarDef {
    pub id: NarrativeVarId,
    pub kind: NarrativeVarKind,
    pub scope: NarrativeScope,
    pub visibility: NarrativeVisibility,
    /// Ids this variable used to be called. A persisted row under an old id is
    /// read through to this one and rewritten under the new id on the next
    /// save (spec §9.3) — the graceful rename path Mass Effect never had.
    #[serde(default)]
    pub renamed_from: Vec<NarrativeVarId>,
    /// How this variable's value is computed, when it is not written directly.
    #[serde(default)]
    pub derived: Option<DerivedRule>,
    /// Free-text, **mandatory**: which chapter/beat this belongs to and what
    /// it means. Mass Effect shipped bare integer PlotIDs with no names at all
    /// and the community had to reverse-engineer the whole mapping; the
    /// manifest, not the save, is this system's source of meaning (spec §3.2).
    pub note: String,
}

impl NarrativeVarDef {
    pub fn is_derived(&self) -> bool { self.derived.is_some() }
}

/// The catalogue of every narrative variable in the game, indexed by id.
///
/// Fields are private so the indices can never drift out of sync with the
/// `Vec` — the same discipline `SpellCompendium` already enforces. Read it
/// through [`Self::get`], [`Self::iter`] and [`Self::resolve`].
#[derive(Clone, Debug, Default)]
pub struct NarrativeManifest {
    vars: Vec<NarrativeVarDef>,
    /// Live `id` -> index into `vars`.
    by_id: HashMap<NarrativeVarId, usize>,
    /// `renamed_from` alias -> index into `vars`.
    aliases: HashMap<NarrativeVarId, usize>,
    /// Ids no longer declared, kept so a persisted row under one is recognised
    /// and dropped quietly rather than logged as unknown.
    retired: HashSet<NarrativeVarId>,
}

/// The on-disk shape of `assets/common/narrative/variables.ron`.
#[derive(Deserialize)]
struct RawNarrativeManifest {
    vars: Vec<NarrativeVarDef>,
    #[serde(default)]
    retired: Vec<NarrativeVarId>,
}

impl Asset for NarrativeManifest {
    fn load(cache: &AssetCache, specifier: &SharedString) -> Result<Self, BoxedError> {
        let raw = cache.load::<Ron<RawNarrativeManifest>>(specifier)?;
        let raw = raw.read();
        Self::build(raw.0.vars.clone(), raw.0.retired.clone())
    }
}

impl NarrativeManifest {
    /// Build and validate a manifest. Every check refuses to load rather than
    /// silently misbehaving — `SpellCompendium::load`'s exact posture, and the
    /// reason the flat-namespace and rewritten-history failure modes in spec
    /// §9.3 cannot open quietly.
    fn build(vars: Vec<NarrativeVarDef>, retired: Vec<NarrativeVarId>) -> Result<Self, BoxedError> {
        if vars.len() > MAX_NARRATIVE_VARS {
            return Err(format!(
                "narrative manifest holds {} variables, over the declared MAX_NARRATIVE_VARS of \
                 {MAX_NARRATIVE_VARS}; raise the constant deliberately if that is intended",
                vars.len(),
            )
            .into());
        }

        let mut by_id = HashMap::with_capacity(vars.len());
        for (i, var) in vars.iter().enumerate() {
            // A duplicate id would make one of the two entries unreachable
            // through `get` — and which one wins would depend on file order.
            if by_id.insert(var.id.clone(), i).is_some() {
                return Err(format!("duplicate narrative variable id: {}", var.id).into());
            }
        }

        let mut aliases: HashMap<NarrativeVarId, usize> = HashMap::new();
        for (i, var) in vars.iter().enumerate() {
            for alias in &var.renamed_from {
                if by_id.contains_key(alias) {
                    return Err(format!(
                        "narrative variable {} claims `renamed_from: {alias}`, but {alias} is \
                         itself a live variable",
                        var.id,
                    )
                    .into());
                }
                if let Some(other) = aliases.insert(alias.clone(), i) {
                    return Err(format!(
                        "narrative alias {alias} is claimed by both {} and {}",
                        vars[other].id, var.id,
                    )
                    .into());
                }
            }
        }

        let mut retired_set = HashSet::with_capacity(retired.len());
        for id in retired {
            if by_id.contains_key(&id) {
                return Err(format!(
                    "narrative variable {id} is listed as retired but still declared"
                )
                .into());
            }
            if aliases.contains_key(&id) {
                return Err(format!(
                    "narrative id {id} is listed as retired but is also a `renamed_from` alias"
                )
                .into());
            }
            retired_set.insert(id);
        }

        let manifest = NarrativeManifest {
            vars,
            by_id,
            aliases,
            retired: retired_set,
        };
        manifest.validate_kinds()?;
        manifest.validate_derived()?;
        Ok(manifest)
    }

    fn validate_kinds(&self) -> Result<(), BoxedError> {
        for var in &self.vars {
            match &var.kind {
                NarrativeVarKind::Flag => {},
                NarrativeVarKind::Choice { options } => {
                    if options.is_empty() {
                        return Err(
                            format!("narrative Choice {} declares no options", var.id).into()
                        );
                    }
                    let mut seen = HashSet::with_capacity(options.len());
                    for option in options {
                        if !seen.insert(option.as_str()) {
                            return Err(format!(
                                "narrative Choice {} declares option {option:?} twice",
                                var.id,
                            )
                            .into());
                        }
                    }
                },
                NarrativeVarKind::Tally { min, max, default } => {
                    Self::check_bounds(&var.id, "Tally", *min, *max, *default)?;
                },
                NarrativeVarKind::Standing {
                    min,
                    max,
                    default,
                    tiers,
                } => {
                    Self::check_bounds(&var.id, "Standing", *min, *max, *default)?;
                    if tiers.is_empty() {
                        return Err(
                            format!("narrative Standing {} declares no tiers", var.id).into()
                        );
                    }
                    if tiers.windows(2).any(|w| w[0].at_least >= w[1].at_least) {
                        return Err(format!(
                            "narrative Standing {}'s tiers must ascend strictly by `at_least`",
                            var.id,
                        )
                        .into());
                    }
                    // Every legal value must land in some band, so the lowest
                    // one has to start at or below `min`.
                    if tiers[0].at_least > *min {
                        return Err(format!(
                            "narrative Standing {}'s lowest tier starts at {} but `min` is {min}, \
                             leaving values below it with no tier",
                            var.id, tiers[0].at_least,
                        )
                        .into());
                    }
                },
            }
        }
        Ok(())
    }

    fn check_bounds(
        id: &NarrativeVarId,
        kind: &str,
        min: i32,
        max: i32,
        default: i32,
    ) -> Result<(), BoxedError> {
        if min > max {
            return Err(format!("narrative {kind} {id} declares min {min} above max {max}").into());
        }
        if default < min || default > max {
            return Err(format!(
                "narrative {kind} {id} declares default {default} outside [{min}, {max}]"
            )
            .into());
        }
        Ok(())
    }

    fn validate_derived(&self) -> Result<(), BoxedError> {
        for var in &self.vars {
            let Some(rule) = &var.derived else { continue };
            match rule {
                DerivedRule::MajorityOf {
                    inputs,
                    min_inputs,
                    fallback,
                    tie_breaker,
                } => {
                    let options = self.expect_choice(var, "MajorityOf")?;
                    if inputs.is_empty() {
                        return Err(format!("derived {} declares no inputs", var.id).into());
                    }
                    if *min_inputs == 0 || *min_inputs > inputs.len() {
                        return Err(format!(
                            "derived {}'s `min_inputs` of {min_inputs} is outside 1..={}",
                            var.id,
                            inputs.len(),
                        )
                        .into());
                    }
                    for input in inputs {
                        let input_def = self.expect_input(var, input)?;
                        let Some(input_options) = input_def.kind.options() else {
                            return Err(format!(
                                "derived {} tallies {input}, which is not a Choice",
                                var.id,
                            )
                            .into());
                        };
                        // Majority counts option *names*, so a name an input
                        // can hold but this variable cannot declare would be
                        // uncountable at runtime.
                        for option in input_options {
                            if !options.iter().any(|o| o == option) {
                                return Err(format!(
                                    "derived {} tallies {input}, which can hold option {option:?} \
                                     that {} does not declare",
                                    var.id, var.id,
                                )
                                .into());
                            }
                        }
                    }
                    Self::expect_option(var, options, fallback, "fallback")?;
                    Self::expect_option(var, options, tie_breaker, "tie_breaker")?;
                },
                DerivedRule::SumOf { inputs } => {
                    match var.kind {
                        NarrativeVarKind::Tally { .. } | NarrativeVarKind::Standing { .. } => {},
                        _ => {
                            return Err(format!(
                                "derived {} uses SumOf, which only makes sense on a Tally or \
                                 Standing",
                                var.id,
                            )
                            .into());
                        },
                    }
                    if inputs.is_empty() {
                        return Err(format!("derived {} declares no terms", var.id).into());
                    }
                    for term in inputs {
                        self.validate_derived_condition(var, &term.when)?;
                    }
                },
                DerivedRule::ThresholdOf { input, bands } => {
                    let options = self.expect_choice(var, "ThresholdOf")?;
                    self.expect_input(var, input)?;
                    if bands.is_empty() {
                        return Err(format!("derived {} declares no bands", var.id).into());
                    }
                    if bands.windows(2).any(|w| w[0].at_least <= w[1].at_least) {
                        return Err(format!(
                            "derived {}'s bands must descend strictly by `at_least`; the first \
                             band the input reaches wins",
                            var.id,
                        )
                        .into());
                    }
                    for band in bands {
                        match &band.then {
                            DerivedValue::Const(name) => {
                                Self::expect_option(var, options, name, "band")?;
                            },
                            DerivedValue::FromChoice { input, map } => {
                                let input_def = self.expect_input(var, input)?;
                                let Some(input_options) = input_def.kind.options() else {
                                    return Err(format!(
                                        "derived {} maps from {input}, which is not a Choice",
                                        var.id,
                                    )
                                    .into());
                                };
                                for (from, to) in map {
                                    if !input_options.iter().any(|o| o == from) {
                                        return Err(format!(
                                            "derived {} maps from option {from:?}, which {input} \
                                             does not declare",
                                            var.id,
                                        )
                                        .into());
                                    }
                                    Self::expect_option(var, options, to, "band map")?;
                                }
                            },
                        }
                    }
                },
            }
        }
        self.validate_no_derived_cycles()
    }

    fn expect_choice<'a>(
        &self,
        var: &'a NarrativeVarDef,
        rule: &str,
    ) -> Result<&'a [String], BoxedError> {
        var.kind.options().ok_or_else(|| {
            format!(
                "derived {} uses {rule}, which resolves to an option name and so only makes sense \
                 on a Choice",
                var.id,
            )
            .into()
        })
    }

    fn expect_input(
        &self,
        var: &NarrativeVarDef,
        input: &NarrativeVarId,
    ) -> Result<&NarrativeVarDef, BoxedError> {
        self.get(input.as_str()).ok_or_else(|| {
            format!(
                "derived {} reads {input}, which no manifest entry declares",
                var.id
            )
            .into()
        })
    }

    fn expect_option(
        var: &NarrativeVarDef,
        options: &[String],
        name: &str,
        role: &str,
    ) -> Result<(), BoxedError> {
        if options.iter().any(|o| o == name) {
            Ok(())
        } else {
            Err(format!(
                "derived {}'s {role} names option {name:?}, which it does not declare",
                var.id,
            )
            .into())
        }
    }

    /// Every variable a derived rule's conditions read must resolve, and none
    /// of them may reach into the emergent layer: a derived value is
    /// recomputed from `(manifest, state)` alone, with no NPC in hand to read
    /// a sentiment from.
    fn validate_derived_condition(
        &self,
        var: &NarrativeVarDef,
        condition: &NarrativeCondition,
    ) -> Result<(), BoxedError> {
        for id in condition.variables() {
            self.expect_input(var, id)?;
        }
        if condition.reads_sentiment() {
            return Err(format!(
                "derived {} reads `SentimentAtLeast`, but a derived value is recomputed from the \
                 manifest and the character's own state alone",
                var.id,
            )
            .into());
        }
        Ok(())
    }

    /// Derived variables recompute on read, so a cycle would recurse until the
    /// stack gave out. Refuse at load instead.
    fn validate_no_derived_cycles(&self) -> Result<(), BoxedError> {
        // 0 = unvisited, 1 = on the current path, 2 = cleared.
        let mut state = vec![0u8; self.vars.len()];
        for root in 0..self.vars.len() {
            if state[root] != 0 {
                continue;
            }
            let mut stack = vec![(root, 0usize)];
            state[root] = 1;
            while let Some((index, next)) = stack.pop() {
                let deps = self.derived_inputs(index);
                if next < deps.len() {
                    stack.push((index, next + 1));
                    let dep = deps[next];
                    match state[dep] {
                        1 => {
                            return Err(format!(
                                "derived narrative variable {} is part of a dependency cycle; a \
                                 derived value is recomputed on every read, so a cycle never \
                                 terminates",
                                self.vars[dep].id,
                            )
                            .into());
                        },
                        0 => {
                            state[dep] = 1;
                            stack.push((dep, 0));
                        },
                        _ => {},
                    }
                } else {
                    state[index] = 2;
                }
            }
        }
        Ok(())
    }

    /// Indices of every manifest entry the entry at `index` reads, if derived.
    fn derived_inputs(&self, index: usize) -> Vec<usize> {
        let Some(rule) = self.vars[index].derived.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut push = |id: &NarrativeVarId| {
            if let Some(i) = self.by_id.get(id.as_str()) {
                out.push(*i);
            }
        };
        match rule {
            DerivedRule::MajorityOf { inputs, .. } => inputs.iter().for_each(&mut push),
            DerivedRule::SumOf { inputs } => {
                for term in inputs {
                    term.when.variables().for_each(&mut push);
                }
            },
            DerivedRule::ThresholdOf { input, bands } => {
                push(input);
                for band in bands {
                    if let DerivedValue::FromChoice { input, .. } = &band.then {
                        push(input);
                    }
                }
            },
        }
        out
    }

    pub fn iter(&self) -> impl Iterator<Item = &NarrativeVarDef> { self.vars.iter() }

    pub fn len(&self) -> usize { self.vars.len() }

    pub fn is_empty(&self) -> bool { self.vars.is_empty() }

    /// The entry declared under exactly this id. Does **not** follow renames —
    /// use [`Self::resolve`] when reading a persisted payload.
    pub fn get(&self, id: &str) -> Option<&NarrativeVarDef> {
        self.by_id.get(id).map(|i| &self.vars[*i])
    }

    /// Resolve a **persisted** id: the live entry declared under it, or the one
    /// that has it in `renamed_from`. The caller writes back under
    /// [`NarrativeVarDef::id`], which is how a rename migrates itself.
    pub fn resolve(&self, id: &str) -> Option<&NarrativeVarDef> {
        self.by_id
            .get(id)
            .or_else(|| self.aliases.get(id))
            .map(|i| &self.vars[*i])
    }

    /// Whether `id` is a deliberately retired variable. A persisted row under
    /// one is dropped quietly instead of being logged as unknown.
    pub fn is_retired(&self, id: &str) -> bool { self.retired.contains(id) }
}

/// The shipped manifest. Panics if the asset is missing or fails validation —
/// deliberately, and at startup: a manifest that does not load means content
/// ids do not mean what the content thinks they mean.
pub fn narrative_manifest() -> AssetReadGuard<NarrativeManifest> {
    NarrativeManifest::load_expect("common.narrative.variables").read()
}

/// Server-authoritative per-character narrative state.
///
/// **Sparse:** a variable the character has never touched has no entry, and
/// reads fall back to the manifest default. `values` is private so it cannot
/// drift from the manifest — every gameplay write goes through [`Self::apply`],
/// which clamps, refuses derived variables, and enforces the kind/effect
/// pairing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NarrativeState {
    values: HashMap<NarrativeVarId, i32>,
}

impl Component for NarrativeState {
    /// Rare: only player characters ever carry one.
    type Storage = DerefFlaggedStorage<Self, HashMapStorage<Self>>;
}

/// Why [`NarrativeState::apply`] refused an effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectRefusal {
    /// No manifest entry declares this id (nor has it as a `renamed_from`).
    UnknownVariable,
    /// Derived variables are recomputed on read and never written.
    Derived,
    /// This effect does not apply to this variable's kind — e.g. `Set` on a
    /// `Tally`, or `Add` on a `Flag`.
    WrongKind,
    /// A `SetChoice` naming an option the variable does not declare.
    UnknownOption,
}

/// What [`NarrativeState::apply`] did with one effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectOutcome {
    /// Written to this character's state. `changed` is false when the stored
    /// value was already the target — every effect but `Add` is idempotent or
    /// monotonic, which is what makes a replayed effect harmless (spec §5.4).
    Applied {
        changed: bool,
    },
    /// Not per-character narrative state: a `World`-scope variable (rtsim's
    /// `world_narrative`) or a `Sentiment` nudge (the emergent layer). Handed
    /// back rather than silently dropped, so the server-side mutation seam has
    /// to route it.
    Deferred,
    Refused(EffectRefusal),
}

impl NarrativeState {
    pub fn is_empty(&self) -> bool { self.values.is_empty() }

    pub fn len(&self) -> usize { self.values.len() }

    /// Every stored (id, value) pair, for persistence. Sparse by construction:
    /// only deviations appear.
    pub fn iter(&self) -> impl Iterator<Item = (&NarrativeVarId, i32)> {
        self.values.iter().map(|(id, v)| (id, *v))
    }

    /// Insert a raw stored value without validating it against the manifest.
    ///
    /// For the persistence loader only, which has already resolved the id
    /// through [`NarrativeManifest::resolve`] and clamped the value. Gameplay
    /// writes go through [`Self::apply`].
    pub fn insert_raw(&mut self, id: NarrativeVarId, value: i32) { self.values.insert(id, value); }

    /// Whether this character has a stored entry for `id`.
    ///
    /// Uniformly "there is a row", for every kind — the same question WoW's
    /// sparse `character_reputation` answers by the row's existence. `Clear`
    /// removes the row, so `Set` then `Clear` reads as unset again.
    pub fn is_set(&self, id: &str) -> bool { self.values.contains_key(id) }

    /// The value of `id` for this character: recomputed if derived, else the
    /// stored value, else the manifest default.
    ///
    /// An id the manifest does not declare reads as `0`; callers that care
    /// should check [`NarrativeManifest::get`] first.
    pub fn get(&self, manifest: &NarrativeManifest, id: &str) -> i32 {
        let Some(def) = manifest.get(id) else {
            return 0;
        };
        self.value_of(manifest, def)
    }

    fn value_of(&self, manifest: &NarrativeManifest, def: &NarrativeVarDef) -> i32 {
        match &def.derived {
            Some(rule) => self.derive(manifest, def, rule),
            None => self
                .values
                .get(def.id.as_str())
                .copied()
                .unwrap_or_else(|| def.kind.default_value()),
        }
    }

    /// The named band `id`'s current value falls in. `None` unless `id` is a
    /// declared `Standing`.
    pub fn tier<'a>(&self, manifest: &'a NarrativeManifest, id: &str) -> Option<&'a str> {
        let def = manifest.get(id)?;
        let NarrativeVarKind::Standing { tiers, .. } = &def.kind else {
            return None;
        };
        let value = self.value_of(manifest, def);
        // Tiers ascend and the lowest starts at or below `min` (enforced at
        // load), so the last one the value reaches is its band.
        tiers
            .iter()
            .rev()
            .find(|tier| value >= tier.at_least)
            .map(|tier| tier.name.as_str())
    }

    /// The option name `id` currently holds. `None` unless `id` is a declared
    /// `Choice` **and** the character has recorded one (or it is derived, which
    /// always resolves).
    pub fn choice<'a>(&self, manifest: &'a NarrativeManifest, id: &str) -> Option<&'a str> {
        let def = manifest.get(id)?;
        let options = def.kind.options()?;
        if !def.is_derived() && !self.is_set(id) {
            return None;
        }
        let index = self.value_of(manifest, def);
        usize::try_from(index)
            .ok()
            .and_then(|i| options.get(i))
            .map(String::as_str)
    }

    /// Apply one authored effect. The single mutating entry point.
    ///
    /// Effects are committed at the instant of the choice, never accumulated
    /// across a conversation — an in-flight conversation is a `#[serde(skip)]`
    /// closure tree that survives nothing (spec §5.6).
    pub fn apply(
        &mut self,
        manifest: &NarrativeManifest,
        effect: &NarrativeEffect,
    ) -> EffectOutcome {
        // A sentiment nudge names no narrative variable at all: it belongs to
        // the emergent layer, which this store does not own.
        let Some(id) = effect.variable() else {
            return EffectOutcome::Deferred;
        };
        let Some(def) = manifest.resolve(id.as_str()) else {
            return EffectOutcome::Refused(EffectRefusal::UnknownVariable);
        };
        if def.is_derived() {
            return EffectOutcome::Refused(EffectRefusal::Derived);
        }
        if def.scope == NarrativeScope::World {
            return EffectOutcome::Deferred;
        }
        self.write(def, effect)
    }

    fn write(&mut self, def: &NarrativeVarDef, effect: &NarrativeEffect) -> EffectOutcome {
        let numeric = matches!(
            def.kind,
            NarrativeVarKind::Tally { .. } | NarrativeVarKind::Standing { .. }
        );
        let current = self
            .values
            .get(def.id.as_str())
            .copied()
            .unwrap_or_else(|| def.kind.default_value());
        let target = match effect {
            NarrativeEffect::Set(_) => {
                if !matches!(def.kind, NarrativeVarKind::Flag) {
                    return EffectOutcome::Refused(EffectRefusal::WrongKind);
                }
                Some(1)
            },
            // `Clear` removes the row rather than storing a zero, on every
            // kind: the store stays sparse and `is_set` stays honest.
            NarrativeEffect::Clear(_) => None,
            NarrativeEffect::SetValue(_, value) => {
                if !numeric {
                    return EffectOutcome::Refused(EffectRefusal::WrongKind);
                }
                Some(def.kind.clamp(*value))
            },
            NarrativeEffect::SetChoice(_, name) => {
                let Some(options) = def.kind.options() else {
                    return EffectOutcome::Refused(EffectRefusal::WrongKind);
                };
                match options.iter().position(|o| o == name) {
                    Some(index) => Some(index as i32),
                    None => return EffectOutcome::Refused(EffectRefusal::UnknownOption),
                }
            },
            NarrativeEffect::Add(_, delta) => {
                if !numeric {
                    return EffectOutcome::Refused(EffectRefusal::WrongKind);
                }
                Some(def.kind.clamp(current.saturating_add(*delta)))
            },
            NarrativeEffect::AtLeastValue(_, value) => {
                if !numeric {
                    return EffectOutcome::Refused(EffectRefusal::WrongKind);
                }
                Some(current.max(def.kind.clamp(*value)))
            },
            NarrativeEffect::Sentiment { .. } => return EffectOutcome::Deferred,
        };

        let changed = match target {
            Some(value) => self.values.insert(def.id.clone(), value) != Some(value),
            None => self.values.remove(def.id.as_str()).is_some(),
        };
        EffectOutcome::Applied { changed }
    }

    fn derive(
        &self,
        manifest: &NarrativeManifest,
        def: &NarrativeVarDef,
        rule: &DerivedRule,
    ) -> i32 {
        match rule {
            DerivedRule::MajorityOf {
                inputs,
                min_inputs,
                fallback,
                tie_breaker,
            } => {
                let options = def.kind.options().unwrap_or_default();
                let mut tally: HashMap<&str, usize> = HashMap::new();
                let mut set = 0usize;
                for input in inputs {
                    if let Some(name) = self.choice(manifest, input.as_str()) {
                        set += 1;
                        *tally.entry(name).or_default() += 1;
                    }
                }
                let winner = if set < *min_inputs {
                    fallback.as_str()
                } else {
                    let best = tally.values().copied().max().unwrap_or(0);
                    let leaders = tally.values().filter(|c| **c == best).count();
                    if leaders == 1 {
                        tally
                            .iter()
                            .find(|(_, c)| **c == best)
                            .map(|(name, _)| *name)
                            .unwrap_or(fallback.as_str())
                    } else {
                        tie_breaker.as_str()
                    }
                };
                options.iter().position(|o| o == winner).unwrap_or(0) as i32
            },
            DerivedRule::SumOf { inputs } => {
                let total = inputs.iter().fold(0i32, |acc, term| {
                    if term.when.evaluate(manifest, self, None) {
                        acc.saturating_add(term.weight)
                    } else {
                        acc
                    }
                });
                def.kind.clamp(total)
            },
            DerivedRule::ThresholdOf { input, bands } => {
                let options = def.kind.options().unwrap_or_default();
                let value = self.get(manifest, input.as_str());
                let name = bands
                    .iter()
                    .find(|band| value >= band.at_least)
                    .map(|band| match &band.then {
                        DerivedValue::Const(name) => name.as_str(),
                        DerivedValue::FromChoice { input, map } => self
                            .choice(manifest, input.as_str())
                            .and_then(|held| map.get(held))
                            .map(String::as_str)
                            // No mapping for what the input holds (or it holds
                            // nothing) is not an error: the band simply does
                            // not resolve, and option 0 is the declared floor.
                            .unwrap_or_default(),
                    })
                    .unwrap_or_default();
                options.iter().position(|o| o == name).unwrap_or(0) as i32
            },
        }
    }
}

/// The bearer conditions authored content can gate on.
///
/// Deliberately shaped like `ConditionPredicate` (`comp/item_condition.rs`),
/// the repo's existing RON-authored, server-evaluated predicate idiom, plus
/// `All`/`Any`/`Not`. No branching, no loops — Bethesda's flat condition list
/// is the same expressive level, and anything richer belongs in content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NarrativeCondition {
    /// The character has a stored entry for this variable. See
    /// [`NarrativeState::is_set`].
    IsSet(NarrativeVarId),
    NotSet(NarrativeVarId),
    Equals(NarrativeVarId, i32),
    /// By option **name**, never index — the index is an implementation detail
    /// of the storage layer.
    ChoiceIs(NarrativeVarId, String),
    AtLeast(NarrativeVarId, i32),
    AtMost(NarrativeVarId, i32),
    TierAtLeast(NarrativeVarId, String),
    All(Vec<NarrativeCondition>),
    Any(Vec<NarrativeCondition>),
    Not(Box<NarrativeCondition>),
    /// Escape hatch into the *emergent* layer, read-only: the acting NPC's
    /// existing rtsim sentiment toward this character. Lets authored content
    /// gate on emergent state without duplicating it (spec §8.4).
    ///
    /// The caller supplies that value; with no NPC in hand this reads as
    /// **false**, never as "no opinion is good enough".
    SentimentAtLeast(f32),
}

impl NarrativeCondition {
    /// Whether this condition holds.
    ///
    /// `sentiment` is the acting NPC's rtsim sentiment toward the character,
    /// when there is one — the one piece of state this layer does not own.
    pub fn evaluate(
        &self,
        manifest: &NarrativeManifest,
        state: &NarrativeState,
        sentiment: Option<f32>,
    ) -> bool {
        match self {
            NarrativeCondition::IsSet(id) => state.is_set(id.as_str()),
            NarrativeCondition::NotSet(id) => !state.is_set(id.as_str()),
            NarrativeCondition::Equals(id, value) => state.get(manifest, id.as_str()) == *value,
            NarrativeCondition::ChoiceIs(id, name) => {
                state.choice(manifest, id.as_str()) == Some(name.as_str())
            },
            NarrativeCondition::AtLeast(id, value) => state.get(manifest, id.as_str()) >= *value,
            NarrativeCondition::AtMost(id, value) => state.get(manifest, id.as_str()) <= *value,
            NarrativeCondition::TierAtLeast(id, name) => {
                let Some(def) = manifest.get(id.as_str()) else {
                    return false;
                };
                let NarrativeVarKind::Standing { tiers, .. } = &def.kind else {
                    return false;
                };
                let Some(wanted) = tiers.iter().position(|t| t.name == *name) else {
                    return false;
                };
                let held = state
                    .tier(manifest, id.as_str())
                    .and_then(|held| tiers.iter().position(|t| t.name == held));
                held.is_some_and(|held| held >= wanted)
            },
            NarrativeCondition::All(conditions) => conditions
                .iter()
                .all(|c| c.evaluate(manifest, state, sentiment)),
            NarrativeCondition::Any(conditions) => conditions
                .iter()
                .any(|c| c.evaluate(manifest, state, sentiment)),
            NarrativeCondition::Not(condition) => !condition.evaluate(manifest, state, sentiment),
            NarrativeCondition::SentimentAtLeast(threshold) => {
                sentiment.is_some_and(|value| value >= *threshold)
            },
        }
    }

    /// Every narrative variable this condition reads, for load-time validation.
    pub fn variables(&self) -> Box<dyn Iterator<Item = &NarrativeVarId> + '_> {
        match self {
            NarrativeCondition::IsSet(id)
            | NarrativeCondition::NotSet(id)
            | NarrativeCondition::Equals(id, _)
            | NarrativeCondition::ChoiceIs(id, _)
            | NarrativeCondition::AtLeast(id, _)
            | NarrativeCondition::AtMost(id, _)
            | NarrativeCondition::TierAtLeast(id, _) => Box::new(std::iter::once(id)),
            NarrativeCondition::All(conditions) | NarrativeCondition::Any(conditions) => {
                Box::new(conditions.iter().flat_map(|c| c.variables()))
            },
            NarrativeCondition::Not(condition) => condition.variables(),
            NarrativeCondition::SentimentAtLeast(_) => Box::new(std::iter::empty()),
        }
    }

    /// Whether this condition reaches into the emergent layer anywhere.
    pub fn reads_sentiment(&self) -> bool {
        match self {
            NarrativeCondition::SentimentAtLeast(_) => true,
            NarrativeCondition::All(conditions) | NarrativeCondition::Any(conditions) => {
                conditions.iter().any(Self::reads_sentiment)
            },
            NarrativeCondition::Not(condition) => condition.reads_sentiment(),
            _ => false,
        }
    }
}

/// Whose emergent sentiment a [`NarrativeEffect::Sentiment`] nudges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SentimentTarget {
    /// The NPC the character is talking to.
    Speaker,
    /// A faction, by its `faction.*` lore id.
    Faction(String),
}

/// The consequences authored content can commit.
///
/// **Every effect is idempotent or explicitly monotonic except `Add`** — which
/// matters because the only thing between a player and a doubled tally is the
/// engine applying an effect twice after a reconnect. An `Add` is expected to
/// be paired with a guard flag by its author.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NarrativeEffect {
    /// `Flag` only.
    Set(NarrativeVarId),
    /// Any kind: removes the stored row, returning the variable to its
    /// manifest default.
    Clear(NarrativeVarId),
    /// `Tally`/`Standing` only. Clamped to the declared bounds.
    SetValue(NarrativeVarId, i32),
    /// `Choice` only, by option name.
    SetChoice(NarrativeVarId, String),
    /// `Tally`/`Standing` only. Saturating, then clamped to the declared
    /// bounds. **The one non-idempotent effect.**
    Add(NarrativeVarId, i32),
    /// `Tally`/`Standing` only. Monotonic high-water mark — Bethesda's
    /// `GetStage` semantics, the shape a "furthest reached" tally wants, and
    /// the one that is safe to apply twice.
    AtLeastValue(NarrativeVarId, i32),
    /// Optional nudge to the *emergent* layer (rtsim `Sentiments`), which this
    /// store does not own: [`NarrativeState::apply`] reports it as
    /// [`EffectOutcome::Deferred`] for the server-side seam to route.
    Sentiment {
        toward: SentimentTarget,
        change: f32,
        cap: f32,
    },
}

impl NarrativeEffect {
    /// The variable this effect writes, when it writes one.
    pub fn variable(&self) -> Option<&NarrativeVarId> {
        match self {
            NarrativeEffect::Set(id)
            | NarrativeEffect::Clear(id)
            | NarrativeEffect::SetValue(id, _)
            | NarrativeEffect::SetChoice(id, _)
            | NarrativeEffect::Add(id, _)
            | NarrativeEffect::AtLeastValue(id, _) => Some(id),
            NarrativeEffect::Sentiment { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(id: &str, kind: NarrativeVarKind) -> NarrativeVarDef {
        NarrativeVarDef {
            id: NarrativeVarId::new(id),
            kind,
            scope: NarrativeScope::Character,
            visibility: NarrativeVisibility::Journal,
            renamed_from: Vec::new(),
            derived: None,
            note: "test".to_string(),
        }
    }

    fn choice(id: &str, options: &[&str]) -> NarrativeVarDef {
        var(id, NarrativeVarKind::Choice {
            options: options.iter().map(|o| o.to_string()).collect(),
        })
    }

    fn tally(id: &str, min: i32, max: i32) -> NarrativeVarDef {
        var(id, NarrativeVarKind::Tally {
            min,
            max,
            default: min,
        })
    }

    fn standing(id: &str) -> NarrativeVarDef {
        var(id, NarrativeVarKind::Standing {
            min: -100,
            max: 100,
            default: 0,
            tiers: vec![
                StandingTier {
                    at_least: -100,
                    name: "reviled".into(),
                },
                StandingTier {
                    at_least: -10,
                    name: "neutral".into(),
                },
                StandingTier {
                    at_least: 25,
                    name: "respected".into(),
                },
            ],
        })
    }

    fn manifest(vars: Vec<NarrativeVarDef>) -> NarrativeManifest {
        NarrativeManifest::build(vars, Vec::new()).expect("a valid manifest")
    }

    // ---- manifest validation ----

    #[test]
    fn duplicate_var_id_refuses_to_load() {
        let err = NarrativeManifest::build(
            vec![
                var("quest.a.flag", NarrativeVarKind::Flag),
                var("quest.a.flag", NarrativeVarKind::Flag),
            ],
            Vec::new(),
        )
        .expect_err("a duplicate id must refuse to load");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn a_renamed_from_alias_colliding_with_a_live_id_refuses_to_load() {
        let mut renamed = var("quest.a.new", NarrativeVarKind::Flag);
        renamed.renamed_from = vec![NarrativeVarId::new("quest.a.old")];
        let err = NarrativeManifest::build(
            vec![renamed, var("quest.a.old", NarrativeVarKind::Flag)],
            Vec::new(),
        )
        .expect_err("an alias shadowing a live id must refuse to load");
        assert!(err.to_string().contains("live variable"));
    }

    #[test]
    fn two_variables_claiming_the_same_alias_refuse_to_load() {
        let mut a = var("quest.a", NarrativeVarKind::Flag);
        a.renamed_from = vec![NarrativeVarId::new("quest.old")];
        let mut b = var("quest.b", NarrativeVarKind::Flag);
        b.renamed_from = vec![NarrativeVarId::new("quest.old")];
        assert!(NarrativeManifest::build(vec![a, b], Vec::new()).is_err());
    }

    #[test]
    fn unsorted_standing_tiers_refuse_to_load() {
        let bad = var("faction.x.standing", NarrativeVarKind::Standing {
            min: -100,
            max: 100,
            default: 0,
            tiers: vec![
                StandingTier {
                    at_least: 25,
                    name: "respected".into(),
                },
                StandingTier {
                    at_least: -100,
                    name: "reviled".into(),
                },
            ],
        });
        assert!(NarrativeManifest::build(vec![bad], Vec::new()).is_err());
    }

    #[test]
    fn a_standing_whose_lowest_tier_is_above_min_refuses_to_load() {
        let bad = var("faction.x.standing", NarrativeVarKind::Standing {
            min: -100,
            max: 100,
            default: 0,
            tiers: vec![StandingTier {
                at_least: -50,
                name: "reviled".into(),
            }],
        });
        let err = NarrativeManifest::build(vec![bad], Vec::new())
            .expect_err("a value below every tier must refuse to load");
        assert!(err.to_string().contains("no tier"));
    }

    #[test]
    fn a_default_outside_its_own_bounds_refuses_to_load() {
        let bad = var("quest.a.tally", NarrativeVarKind::Tally {
            min: 0,
            max: 3,
            default: 9,
        });
        assert!(NarrativeManifest::build(vec![bad], Vec::new()).is_err());
    }

    #[test]
    fn a_manifest_over_the_declared_ceiling_refuses_to_load() {
        let vars = (0..=MAX_NARRATIVE_VARS)
            .map(|i| var(&format!("quest.bulk.v{i}"), NarrativeVarKind::Flag))
            .collect();
        let err = NarrativeManifest::build(vars, Vec::new())
            .expect_err("over MAX_NARRATIVE_VARS must refuse to load");
        assert!(err.to_string().contains("MAX_NARRATIVE_VARS"));
    }

    #[test]
    fn an_unresolvable_derived_input_refuses_to_load() {
        let mut derived = choice("quest.a.outcome", &["x", "y"]);
        derived.derived = Some(DerivedRule::MajorityOf {
            inputs: vec![NarrativeVarId::new("quest.a.nonexistent")],
            min_inputs: 1,
            fallback: "x".into(),
            tie_breaker: "y".into(),
        });
        let err = NarrativeManifest::build(vec![derived], Vec::new())
            .expect_err("an unresolvable derived input must refuse to load");
        assert!(err.to_string().contains("no manifest entry declares"));
    }

    #[test]
    fn a_derived_cycle_refuses_to_load() {
        let mut a = choice("quest.a", &["x", "y"]);
        let mut b = choice("quest.b", &["x", "y"]);
        a.derived = Some(DerivedRule::MajorityOf {
            inputs: vec![NarrativeVarId::new("quest.b")],
            min_inputs: 1,
            fallback: "x".into(),
            tie_breaker: "y".into(),
        });
        b.derived = Some(DerivedRule::MajorityOf {
            inputs: vec![NarrativeVarId::new("quest.a")],
            min_inputs: 1,
            fallback: "x".into(),
            tie_breaker: "y".into(),
        });
        let err = NarrativeManifest::build(vec![a, b], Vec::new())
            .expect_err("a derived cycle must refuse to load");
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn a_derived_rule_reading_the_emergent_layer_refuses_to_load() {
        let mut derived = tally("quest.a.footing", 0, 3);
        derived.derived = Some(DerivedRule::SumOf {
            inputs: vec![SumTerm {
                when: NarrativeCondition::SentimentAtLeast(0.5),
                weight: 1,
            }],
        });
        let err = NarrativeManifest::build(vec![derived], Vec::new())
            .expect_err("a derived rule cannot read sentiment");
        assert!(err.to_string().contains("SentimentAtLeast"));
    }

    #[test]
    fn a_majority_input_holding_an_undeclared_option_refuses_to_load() {
        let input = choice("quest.a.point", &["x", "y", "z"]);
        let mut derived = choice("quest.a.outcome", &["x", "y"]);
        derived.derived = Some(DerivedRule::MajorityOf {
            inputs: vec![NarrativeVarId::new("quest.a.point")],
            min_inputs: 1,
            fallback: "x".into(),
            tie_breaker: "y".into(),
        });
        assert!(NarrativeManifest::build(vec![input, derived], Vec::new()).is_err());
    }

    // ---- reads ----

    #[test]
    fn an_untouched_variable_reads_its_manifest_default() {
        let m = manifest(vec![
            tally("quest.a.tally", 2, 9),
            standing("faction.x.standing"),
        ]);
        let state = NarrativeState::default();
        assert_eq!(state.get(&m, "quest.a.tally"), 2);
        assert_eq!(state.get(&m, "faction.x.standing"), 0);
        assert_eq!(state.tier(&m, "faction.x.standing"), Some("neutral"));
        assert!(!state.is_set("quest.a.tally"));
    }

    #[test]
    fn an_unrecorded_choice_reads_as_no_choice_rather_than_option_zero() {
        let m = manifest(vec![choice("quest.a.spine", &["hold", "cut"])]);
        let mut state = NarrativeState::default();
        assert_eq!(state.choice(&m, "quest.a.spine"), None);
        state.apply(
            &m,
            &NarrativeEffect::SetChoice(NarrativeVarId::new("quest.a.spine"), "hold".into()),
        );
        assert_eq!(state.choice(&m, "quest.a.spine"), Some("hold"));
    }

    #[test]
    fn tier_at_least_ranks_bands_rather_than_comparing_names() {
        let m = manifest(vec![standing("faction.x.standing")]);
        let mut state = NarrativeState::default();
        let id = NarrativeVarId::new("faction.x.standing");
        let respected = NarrativeCondition::TierAtLeast(id.clone(), "respected".into());
        assert!(!respected.evaluate(&m, &state, None));
        state.apply(&m, &NarrativeEffect::SetValue(id.clone(), 30));
        assert!(respected.evaluate(&m, &state, None));
        assert_eq!(state.tier(&m, "faction.x.standing"), Some("respected"));
        assert!(
            NarrativeCondition::TierAtLeast(id, "reviled".into()).evaluate(&m, &state, None),
            "a higher band must satisfy a lower one"
        );
    }

    #[test]
    fn sentiment_at_least_is_false_with_no_npc_in_hand() {
        let m = manifest(vec![]);
        let state = NarrativeState::default();
        let c = NarrativeCondition::SentimentAtLeast(0.0);
        assert!(!c.evaluate(&m, &state, None), "absent must not read as met");
        assert!(c.evaluate(&m, &state, Some(0.1)));
    }

    // ---- writes ----

    #[test]
    fn add_clamps_to_the_declared_bounds_and_saturates() {
        let m = manifest(vec![tally("quest.a.tally", 0, 3)]);
        let id = NarrativeVarId::new("quest.a.tally");
        let mut state = NarrativeState::default();
        for _ in 0..10 {
            state.apply(&m, &NarrativeEffect::Add(id.clone(), 1));
        }
        assert_eq!(state.get(&m, "quest.a.tally"), 3);
        state.apply(&m, &NarrativeEffect::Add(id.clone(), i32::MIN));
        assert_eq!(
            state.get(&m, "quest.a.tally"),
            0,
            "a saturating add must land on the floor, not wrap"
        );
        state.apply(&m, &NarrativeEffect::Add(id, i32::MAX));
        assert_eq!(state.get(&m, "quest.a.tally"), 3);
    }

    #[test]
    fn at_least_value_is_a_monotonic_high_water_mark_and_is_idempotent() {
        let m = manifest(vec![tally("quest.a.reached", 0, 10)]);
        let id = NarrativeVarId::new("quest.a.reached");
        let mut state = NarrativeState::default();
        assert_eq!(
            state.apply(&m, &NarrativeEffect::AtLeastValue(id.clone(), 7)),
            EffectOutcome::Applied { changed: true }
        );
        assert_eq!(
            state.apply(&m, &NarrativeEffect::AtLeastValue(id.clone(), 7)),
            EffectOutcome::Applied { changed: false },
            "applying it twice must be a no-op"
        );
        state.apply(&m, &NarrativeEffect::AtLeastValue(id.clone(), 3));
        assert_eq!(
            state.get(&m, "quest.a.reached"),
            7,
            "a lower value must never walk the mark back"
        );
        state.apply(&m, &NarrativeEffect::AtLeastValue(id, 99));
        assert_eq!(state.get(&m, "quest.a.reached"), 10, "still clamped");
    }

    #[test]
    fn set_and_clear_keep_the_store_sparse() {
        let m = manifest(vec![var("quest.a.met", NarrativeVarKind::Flag)]);
        let id = NarrativeVarId::new("quest.a.met");
        let mut state = NarrativeState::default();
        assert!(state.is_empty());
        state.apply(&m, &NarrativeEffect::Set(id.clone()));
        assert!(state.is_set("quest.a.met"));
        assert_eq!(state.len(), 1);
        state.apply(&m, &NarrativeEffect::Clear(id));
        assert!(
            state.is_empty(),
            "Clear must remove the row, not store a zero"
        );
    }

    #[test]
    fn an_effect_that_does_not_match_the_kind_is_refused_rather_than_coerced() {
        let m = manifest(vec![
            var("quest.a.flag", NarrativeVarKind::Flag),
            tally("quest.a.tally", 0, 3),
            choice("quest.a.choice", &["x"]),
        ]);
        let mut state = NarrativeState::default();
        assert_eq!(
            state.apply(
                &m,
                &NarrativeEffect::Add(NarrativeVarId::new("quest.a.flag"), 1)
            ),
            EffectOutcome::Refused(EffectRefusal::WrongKind)
        );
        assert_eq!(
            state.apply(
                &m,
                &NarrativeEffect::Set(NarrativeVarId::new("quest.a.tally"))
            ),
            EffectOutcome::Refused(EffectRefusal::WrongKind)
        );
        assert_eq!(
            state.apply(
                &m,
                &NarrativeEffect::SetChoice(
                    NarrativeVarId::new("quest.a.choice"),
                    "nope".to_string()
                )
            ),
            EffectOutcome::Refused(EffectRefusal::UnknownOption)
        );
        assert_eq!(
            state.apply(
                &m,
                &NarrativeEffect::Set(NarrativeVarId::new("quest.a.missing"))
            ),
            EffectOutcome::Refused(EffectRefusal::UnknownVariable)
        );
        assert!(state.is_empty(), "a refused effect must write nothing");
    }

    #[test]
    fn a_derived_variable_is_never_written_directly() {
        let input = choice("quest.a.point", &["x", "y"]);
        let mut derived = choice("quest.a.outcome", &["x", "y"]);
        derived.derived = Some(DerivedRule::MajorityOf {
            inputs: vec![NarrativeVarId::new("quest.a.point")],
            min_inputs: 1,
            fallback: "x".into(),
            tie_breaker: "y".into(),
        });
        let m = manifest(vec![input, derived]);
        let mut state = NarrativeState::default();
        assert_eq!(
            state.apply(
                &m,
                &NarrativeEffect::SetChoice(
                    NarrativeVarId::new("quest.a.outcome"),
                    "y".to_string()
                )
            ),
            EffectOutcome::Refused(EffectRefusal::Derived)
        );
        assert!(state.is_empty());
    }

    #[test]
    fn a_world_scope_write_is_deferred_rather_than_stored_per_character() {
        let mut world = var("quest.a.revealed", NarrativeVarKind::Flag);
        world.scope = NarrativeScope::World;
        let m = manifest(vec![world]);
        let mut state = NarrativeState::default();
        assert_eq!(
            state.apply(
                &m,
                &NarrativeEffect::Set(NarrativeVarId::new("quest.a.revealed"))
            ),
            EffectOutcome::Deferred
        );
        assert!(state.is_empty());
    }

    #[test]
    fn a_sentiment_effect_is_deferred_rather_than_silently_dropped() {
        let m = manifest(vec![]);
        let mut state = NarrativeState::default();
        assert_eq!(
            state.apply(&m, &NarrativeEffect::Sentiment {
                toward: SentimentTarget::Speaker,
                change: 0.1,
                cap: 0.5,
            }),
            EffectOutcome::Deferred
        );
    }

    #[test]
    fn a_write_under_an_old_id_lands_on_the_variable_that_renamed_it() {
        let mut renamed = var("quest.a.new", NarrativeVarKind::Flag);
        renamed.renamed_from = vec![NarrativeVarId::new("quest.a.old")];
        let m = manifest(vec![renamed]);
        let mut state = NarrativeState::default();
        state.apply(
            &m,
            &NarrativeEffect::Set(NarrativeVarId::new("quest.a.old")),
        );
        assert!(
            state.is_set("quest.a.new"),
            "a rename must be written back under the live id"
        );
        assert!(!state.is_set("quest.a.old"));
    }

    // ---- derived rules ----

    fn hollow_choir() -> NarrativeManifest {
        let mut outcome = choice("quest.a.outcome", &["rot_queen", "faceless_lord", "hold"]);
        outcome.derived = Some(DerivedRule::MajorityOf {
            inputs: vec![
                NarrativeVarId::new("quest.a.spine"),
                NarrativeVarId::new("quest.a.sluice"),
                NarrativeVarId::new("quest.a.last_voice"),
            ],
            min_inputs: 2,
            fallback: "faceless_lord".into(),
            tie_breaker: "hold".into(),
        });
        manifest(vec![
            choice("quest.a.spine", &["rot_queen", "faceless_lord", "hold"]),
            choice("quest.a.sluice", &["rot_queen", "faceless_lord", "hold"]),
            choice("quest.a.last_voice", &[
                "rot_queen",
                "faceless_lord",
                "hold",
            ]),
            outcome,
        ])
    }

    fn pick(m: &NarrativeManifest, state: &mut NarrativeState, id: &str, option: &str) {
        assert_eq!(
            state.apply(
                m,
                &NarrativeEffect::SetChoice(NarrativeVarId::new(id), option.to_string())
            ),
            EffectOutcome::Applied { changed: true }
        );
    }

    #[test]
    fn majority_of_falls_back_when_too_few_inputs_are_set() {
        let m = hollow_choir();
        let mut state = NarrativeState::default();
        assert_eq!(
            state.choice(&m, "quest.a.outcome"),
            Some("faceless_lord"),
            "inaction has a named consequence because the rule declares one"
        );
        pick(&m, &mut state, "quest.a.spine", "hold");
        assert_eq!(
            state.choice(&m, "quest.a.outcome"),
            Some("faceless_lord"),
            "one input is still under min_inputs"
        );
    }

    #[test]
    fn majority_of_takes_the_plurality_and_breaks_ties_as_declared() {
        let m = hollow_choir();
        let mut state = NarrativeState::default();
        pick(&m, &mut state, "quest.a.spine", "hold");
        pick(&m, &mut state, "quest.a.sluice", "hold");
        assert_eq!(state.choice(&m, "quest.a.outcome"), Some("hold"));

        let mut split = NarrativeState::default();
        pick(&m, &mut split, "quest.a.spine", "rot_queen");
        pick(&m, &mut split, "quest.a.sluice", "faceless_lord");
        assert_eq!(
            split.choice(&m, "quest.a.outcome"),
            Some("hold"),
            "a tie must resolve to the declared tie_breaker, never to option order"
        );
    }

    #[test]
    fn a_derived_value_is_recomputed_on_read_and_never_stored() {
        let m = hollow_choir();
        let mut state = NarrativeState::default();
        pick(&m, &mut state, "quest.a.spine", "rot_queen");
        pick(&m, &mut state, "quest.a.sluice", "rot_queen");
        assert_eq!(state.choice(&m, "quest.a.outcome"), Some("rot_queen"));
        assert!(
            !state.is_set("quest.a.outcome"),
            "a derived variable must never occupy a stored row"
        );
        pick(&m, &mut state, "quest.a.last_voice", "hold");
        // Changing an input changes the outcome with no write anywhere.
        assert_eq!(state.choice(&m, "quest.a.outcome"), Some("rot_queen"));
        assert_eq!(state.len(), 3);
    }

    #[test]
    fn sum_of_counts_the_terms_that_hold_and_clamps_to_its_own_bounds() {
        let mut footing = tally("quest.a.footing", 0, 3);
        footing.derived = Some(DerivedRule::SumOf {
            inputs: vec![
                SumTerm {
                    when: NarrativeCondition::Not(Box::new(NarrativeCondition::ChoiceIs(
                        NarrativeVarId::new("quest.a.backing"),
                        "none".into(),
                    ))),
                    weight: 1,
                },
                SumTerm {
                    when: NarrativeCondition::Equals(NarrativeVarId::new("quest.a.components"), 5),
                    weight: 1,
                },
                SumTerm {
                    when: NarrativeCondition::IsSet(NarrativeVarId::new("quest.a.met")),
                    weight: 4,
                },
            ],
        });
        let m = manifest(vec![
            choice("quest.a.backing", &["crownguard", "none"]),
            tally("quest.a.components", 0, 5),
            var("quest.a.met", NarrativeVarKind::Flag),
            footing,
        ]);

        let mut state = NarrativeState::default();
        assert_eq!(
            state.get(&m, "quest.a.footing"),
            1,
            "an unrecorded backing is not \"none\", so the term holds"
        );
        pick(&m, &mut state, "quest.a.backing", "none");
        assert_eq!(state.get(&m, "quest.a.footing"), 0);
        state.apply(
            &m,
            &NarrativeEffect::SetValue(NarrativeVarId::new("quest.a.components"), 5),
        );
        assert_eq!(state.get(&m, "quest.a.footing"), 1);
        state.apply(
            &m,
            &NarrativeEffect::Set(NarrativeVarId::new("quest.a.met")),
        );
        assert_eq!(
            state.get(&m, "quest.a.footing"),
            3,
            "the sum must be clamped to the variable's own bounds"
        );
    }

    #[test]
    fn threshold_of_takes_the_highest_band_the_input_reaches() {
        let mut survivor = choice("quest.a.survivor", &["demogorgon", "rot_queen"]);
        survivor.derived = Some(DerivedRule::ThresholdOf {
            input: NarrativeVarId::new("quest.a.footing"),
            bands: vec![
                ThresholdBand {
                    at_least: 2,
                    then: DerivedValue::FromChoice {
                        input: NarrativeVarId::new("quest.a.outcome"),
                        map: [
                            ("rot_queen".to_string(), "rot_queen".to_string()),
                            ("hold".to_string(), "demogorgon".to_string()),
                        ]
                        .into_iter()
                        .collect(),
                    },
                },
                ThresholdBand {
                    at_least: 0,
                    then: DerivedValue::Const("demogorgon".into()),
                },
            ],
        });
        let m = manifest(vec![
            tally("quest.a.footing", 0, 3),
            choice("quest.a.outcome", &["rot_queen", "hold"]),
            survivor,
        ]);

        let mut state = NarrativeState::default();
        pick(&m, &mut state, "quest.a.outcome", "rot_queen");
        assert_eq!(
            state.choice(&m, "quest.a.survivor"),
            Some("demogorgon"),
            "with no footing, Demogorgon regardless"
        );
        state.apply(
            &m,
            &NarrativeEffect::SetValue(NarrativeVarId::new("quest.a.footing"), 2),
        );
        assert_eq!(state.choice(&m, "quest.a.survivor"), Some("rot_queen"));
    }

    // ---- the shipped manifest ----

    #[test]
    fn the_shipped_manifest_loads_and_validates() {
        let manifest = narrative_manifest();
        assert!(
            !manifest.is_empty(),
            "the manifest must ship real entries so the loader is actually exercised"
        );
        assert!(manifest.len() <= MAX_NARRATIVE_VARS);
    }

    /// Ids live in the lore's own `quest.*` / `faction.*` namespace, which is
    /// already generated into `assets/lore/index.ron`. This closes the
    /// Bethesda / Beyond-Skyrim flat-unowned-namespace failure mode (spec §10)
    /// before it can open: a variable whose adventure or faction does not
    /// exist in canon fails here rather than drifting.
    #[test]
    fn narrative_var_prefixes_resolve_against_the_lore_index() {
        use crate::assets::{AssetExt, Ron};

        #[derive(serde::Deserialize)]
        struct LoreIndex {
            ids: Vec<String>,
        }

        let index = Ron::<LoreIndex>::load_expect("lore.index");
        let index = index.read();
        let ids: HashSet<&str> = index.0.ids.iter().map(String::as_str).collect();
        let manifest = narrative_manifest();

        for var in manifest.iter() {
            let id = var.id.as_str();
            if !id.starts_with("quest.") && !id.starts_with("faction.") {
                continue;
            }
            // The lore leaf is this id or some prefix of it: a variable on
            // `quest.the_kind_work` may be `quest.the_kind_work.commissions`
            // or deeper.
            let resolves =
                ids.contains(id) || id.match_indices('.').any(|(at, _)| ids.contains(&id[..at]));
            assert!(
                resolves,
                "narrative variable {id} has no `quest.*`/`faction.*` ancestor in \
                 assets/lore/index.ron; either the lore leaf is missing or the id is wrong"
            );
        }
    }

    /// A `Choice` stores the option's index within its own list, so reordering
    /// or deleting an option silently rewrites every character's history (spec
    /// §9.3). This pins the shipped option lists: a diff that only *appends*
    /// leaves every existing entry in place and passes; anything else fails
    /// here and has to be a deliberate, reviewed change to this list.
    #[test]
    fn choice_option_lists_are_append_only() {
        // (variable id, its option list as of the commit that shipped it).
        // ONLY ever append to an inner list, and only ever append new rows.
        let shipped: &[(&str, &[&str])] = &[
            ("quest.the_kind_work.tourniquet", &[
                "town",
                "waystation",
                "refused",
            ]),
            ("quest.the_kind_work.offer", &["accepted", "declined"]),
            ("quest.the_kind_work.footing", &[
                "unaligned",
                "trusted",
                "complicit",
            ]),
        ];

        let manifest = narrative_manifest();
        for (id, expected) in shipped {
            let def = manifest
                .get(id)
                .unwrap_or_else(|| panic!("{id} is pinned here but no longer declared"));
            let options = def
                .kind
                .options()
                .unwrap_or_else(|| panic!("{id} is pinned as a Choice but is no longer one"));
            assert!(
                options.len() >= expected.len(),
                "{id} lost options; a Choice option list is append-only"
            );
            for (i, want) in expected.iter().enumerate() {
                assert_eq!(
                    options[i], *want,
                    "{id} option {i} changed from {want:?} to {:?}; a stored index would now mean \
                     something else",
                    options[i],
                );
            }
        }
    }
}
