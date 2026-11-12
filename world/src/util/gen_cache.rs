use hashbrown::HashMap;
use vek::Vec2;

use super::{Sampler, StructureGen2d, structure::StructureField};

pub struct StructureGenCache<T> {
    generation: StructureGen2d,
    // TODO: Compare performance of using binary search instead of hashmap
    cache: HashMap<Vec2<i32>, Option<T>>,
    next_fields: Option<(Vec2<i32>, Vec<StructureField>)>,
}

impl<T> StructureGenCache<T> {
    pub fn new(generation: StructureGen2d) -> Self {
        Self {
            generation,
            cache: HashMap::new(),
            next_fields: None,
        }
    }

    /// Uses an already de-duplicated deterministic candidate sequence for the
    /// next [`Self::get`] at `index`. This lets authored regions augment the
    /// normal lattice without changing the global generator for other worlds.
    pub fn use_fields_for_next_get(&mut self, index: Vec2<i32>, fields: Vec<StructureField>) {
        assert!(
            self.next_fields.is_none(),
            "a structure candidate override was not consumed before the next request"
        );
        self.next_fields = Some((index, fields));
    }

    pub fn get(
        &mut self,
        index: Vec2<i32>,
        mut generate: impl FnMut(Vec2<i32>, u32) -> Option<T>,
    ) -> Vec<&T> {
        let override_fields = self.next_fields.take().map(|(override_index, fields)| {
            assert_eq!(
                override_index, index,
                "a structure candidate override was consumed by a different index"
            );
            fields
        });
        let generated_fields = override_fields
            .is_none()
            .then(|| self.generation.get(index));
        let fields = override_fields
            .as_deref()
            .or_else(|| generated_fields.as_ref().map(|fields| fields.as_slice()))
            .expect("tree candidate fields come from either the override or global generator");
        for (wpos, seed) in fields {
            self.cache
                .entry(*wpos)
                .or_insert_with(|| generate(*wpos, *seed));
        }

        fields
            .iter()
            .filter_map(|(wpos, _)| self.cache.get(wpos).unwrap().as_ref())
            .collect()
    }

    pub fn generated(&self) -> impl Iterator<Item = &T> {
        self.cache.values().filter_map(|v| v.as_ref())
    }
}
