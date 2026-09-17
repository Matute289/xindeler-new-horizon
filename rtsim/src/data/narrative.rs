use common::comp::{NarrativeManifest, NarrativeVarId, NarrativeVarKind};
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
/// Sparse, with exactly the same semantics as the per-character store: a
/// variable nothing has touched has no entry, and reads fall back to the
/// manifest default.
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

    /// The option name `id` currently holds, if it is a declared `Choice` and
    /// the world has recorded one.
    pub fn choice<'a>(&self, manifest: &'a NarrativeManifest, id: &str) -> Option<&'a str> {
        let def = manifest.get(id)?;
        let NarrativeVarKind::Choice { options } = &def.kind else {
            return None;
        };
        let index = *self.values.get(id)?;
        usize::try_from(index)
            .ok()
            .and_then(|i| options.get(i))
            .map(String::as_str)
    }

    /// Store `value` for `id`, clamped to the variable's declared bounds.
    /// Returns whether the stored value moved.
    pub fn set(&mut self, manifest: &NarrativeManifest, id: &NarrativeVarId, value: i32) -> bool {
        let Some(def) = manifest.resolve(id.as_str()) else {
            return false;
        };
        let value = def.kind.clamp(value);
        self.values.insert(def.id.clone(), value) != Some(value)
    }

    /// Remove `id`'s stored entry, returning it to the manifest default.
    /// Returns whether anything was removed.
    pub fn clear(&mut self, id: &str) -> bool { self.values.remove(id).is_some() }

    /// Insert a raw stored value without validating it against the manifest,
    /// for a loader that has already resolved and clamped it.
    pub fn insert_raw(&mut self, id: NarrativeVarId, value: i32) { self.values.insert(id, value); }
}
