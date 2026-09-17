use common::comp::{NarrativeManifest, NarrativeVarId};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// World-scoped narrative variables: facts that became true for *everyone*, as
/// opposed to the per-character record of what one character chose.
///
/// This is the one narrative-state home outside the character database, and it
/// is deliberately the small one. World facts belong here because they
/// genuinely are world state, and because they degrade gracefully: an rtsim
/// purge means the world has merely forgotten that something became public,
/// while the same purge applied to per-character progress would destroy a
/// player's saga. That asymmetry is the whole reason per-character state lives
/// in the character DB instead.
///
/// Sparse, with the same semantics as the per-character store: a variable
/// nothing has touched has no entry, and reads fall back to the manifest
/// default.
///
/// **Read-only for now, deliberately.** Nothing writes a world-scope variable
/// yet — `NarrativeState::apply` hands a `World`-scope effect back as
/// `EffectOutcome::Deferred` for a server-side seam that does not exist. The
/// mutators are left out rather than shipped without a caller, because the
/// routing seam is what should decide their shape.
///
/// 🔴 **When that seam lands, it also owes this store the migration pass the
/// character store already has.** These values are deserialised straight out
/// of the MessagePack save, so unlike `db_string_to_narrative_state` they get
/// no `resolve` (renames), no retired-id sweep, and no `kind.clamp`. That is
/// harmless while the map is always empty; it stops being harmless the moment
/// anything writes to it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorldNarrative {
    values: HashMap<NarrativeVarId, i32>,
}

impl WorldNarrative {
    pub fn is_empty(&self) -> bool { self.values.is_empty() }

    pub fn len(&self) -> usize { self.values.len() }

    pub fn iter(&self) -> impl Iterator<Item = (&NarrativeVarId, i32)> {
        self.values.iter().map(|(id, v)| (id, *v))
    }

    /// Whether the world has a stored entry for `id`.
    pub fn is_set(&self, id: &str) -> bool { self.values.contains_key(id) }

    /// The world's value for `id`, falling back to the manifest default.
    ///
    /// An id the manifest does not declare reads as `0`. Derived variables are
    /// not resolved here: a derived value is a reading of *a character's* own
    /// record, and the world holds no character.
    pub fn get(&self, manifest: &NarrativeManifest, id: &str) -> i32 {
        let Some(def) = manifest.get(id) else {
            return 0;
        };
        self.values
            .get(id)
            .copied()
            .unwrap_or_else(|| def.kind.default_value())
    }
}
