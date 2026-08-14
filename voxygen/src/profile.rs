use crate::hud;
use common::{character::CharacterId, uuid::Uuid};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::warn;

/// Represents a character in the profile.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterProfile {
    /// Array representing a character's hotbar (the 10 numbered slots).
    pub hotbar_slots: [Option<hud::HotbarSlotContents>; 10],
    /// The left/right mouse-click override slots. Kept as a separate field
    /// (rather than folded into `hotbar_slots`) so existing `profile.ron`
    /// files -- which serialize `hotbar_slots` as a fixed 10-element
    /// sequence -- keep deserializing correctly; a missing field falls back
    /// to `default_mouse_slots()` via the struct-level `#[serde(default)]`.
    pub hotbar_mouse_slots: [Option<hud::HotbarSlotContents>; 2],
}

const fn default_slots() -> [Option<hud::HotbarSlotContents>; 10] {
    [None, None, None, None, None, None, None, None, None, None]
}

const fn default_mouse_slots() -> [Option<hud::HotbarSlotContents>; 2] { [None, None] }

impl Default for CharacterProfile {
    fn default() -> Self {
        CharacterProfile {
            hotbar_slots: default_slots(),
            hotbar_mouse_slots: default_mouse_slots(),
        }
    }
}

/// Represents a server in the profile.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerProfile {
    /// A map of character's by id to their CharacterProfile.
    pub characters: HashMap<CharacterId, CharacterProfile>,
    /// Selected character in the chararacter selection screen
    pub selected_character: Option<CharacterId>,
    /// Last spectate position
    pub spectate_position: Option<vek::Vec3<f32>>,
    /// Hash of left-accepted server rules
    pub accepted_rules: Option<u64>,
}

impl Default for ServerProfile {
    fn default() -> Self {
        ServerProfile {
            characters: HashMap::new(),
            selected_character: None,
            spectate_position: None,
            accepted_rules: None,
        }
    }
}

/// `Profile` contains everything that can be configured in the profile.ron
///
/// Initially it is just for persisting things that don't belong in
/// settings.ron - like the state of hotbar and any other character level
/// configuration.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub servers: HashMap<String, ServerProfile>,
    pub mutelist: HashMap<Uuid, String>,
    /// Temporary character profile, used when it should
    /// not be persisted to the disk.
    #[serde(skip)]
    pub transient_character: Option<CharacterProfile>,
    pub tutorial: hud::tutorial::TutorialState,
}

impl Profile {
    /// Load the profile.ron file from the standard path or create it.
    pub fn load(config_dir: &Path) -> Self {
        let path = Profile::get_path(config_dir);

        let profile = common::util::ron_from_path_recoverable::<Self>(&path);
        // Save profile to add new fields or create the file if it is not already there
        profile.save_to_file_warn(config_dir);
        profile
    }

    /// Save the current profile to disk, warn on failure.
    pub fn save_to_file_warn(&self, config_dir: &Path) {
        if let Err(e) = self.save_to_file(config_dir) {
            warn!(?e, "Failed to save profile");
        }
    }

    /// Get the hotbar_slots for the requested character_id.
    ///
    /// If the server or character does not exist then the default hotbar_slots
    /// (empty) is returned.
    ///
    /// # Arguments
    ///
    /// * server - current server the character is on.
    /// * character_id - id of the character, passing `None` indicates the
    ///   transient character profile should be used.
    pub fn get_hotbar_slots(
        &self,
        server: &str,
        character_id: Option<CharacterId>,
    ) -> [Option<hud::HotbarSlotContents>; hud::HOTBAR_SLOT_COUNT] {
        let character = match character_id {
            Some(character_id) => self
                .servers
                .get(server)
                .and_then(|s| s.characters.get(&character_id)),
            None => self.transient_character.as_ref(),
        };
        let numbered = character
            .map(|c| c.hotbar_slots.clone())
            .unwrap_or_else(default_slots);
        let mouse = character
            .map(|c| c.hotbar_mouse_slots.clone())
            .unwrap_or_else(default_mouse_slots);

        let mut combined = std::array::from_fn(|_| None);
        combined[..10].clone_from_slice(&numbered);
        combined[10..].clone_from_slice(&mouse);
        combined
    }

    /// Set the hotbar_slots for the requested character_id.
    ///
    /// If the server or character does not exist then the appropriate fields
    /// will be initialised and the slots added.
    ///
    /// # Arguments
    ///
    /// * server - current server the character is on.
    /// * character_id - id of the character, passing `None` indicates the
    ///   transient character profile should be used.
    /// * slots - array of hotbar_slots to save.
    pub fn set_hotbar_slots(
        &mut self,
        server: &str,
        character_id: Option<CharacterId>,
        slots: [Option<hud::HotbarSlotContents>; hud::HOTBAR_SLOT_COUNT],
    ) {
        let [numbered @ .., mouse_left, mouse_right] = slots;
        let character = match character_id {
            Some(character_id) => self.servers
              .entry(server.to_string())
              .or_default()
              // Get or update the CharacterProfile.
              .characters
              .entry(character_id)
              .or_default(),
            None => self.transient_character.get_or_insert_default(),
        };
        character.hotbar_slots = numbered;
        character.hotbar_mouse_slots = [mouse_left, mouse_right];
    }

    /// Get the selected_character for the provided server.
    ///
    /// if the server does not exist then the default selected_character (None)
    /// is returned.
    ///
    /// # Arguments
    ///
    /// * server - current server the character is on.
    pub fn get_selected_character(&self, server: &str) -> Option<CharacterId> {
        self.servers
            .get(server)
            .map(|s| s.selected_character)
            .unwrap_or_default()
    }

    /// Set the selected_character for the provided server.
    ///
    /// If the server does not exist then the appropriate fields
    /// will be initialised and the selected_character added.
    ///
    /// # Arguments
    ///
    /// * server - current server the character is on.
    /// * selected_character - option containing selected character ID
    pub fn set_selected_character(
        &mut self,
        server: &str,
        selected_character: Option<CharacterId>,
    ) {
        self.servers
            .entry(server.to_string())
            .or_default()
            .selected_character = selected_character;
    }

    /// Get the selected_character for the provided server.
    ///
    /// if the server does not exist then the default spectate_position (None)
    /// is returned.
    ///
    /// # Arguments
    ///
    /// * server - current server the player is on.
    pub fn get_spectate_position(&self, server: &str) -> Option<vek::Vec3<f32>> {
        self.servers
            .get(server)
            .map(|s| s.spectate_position)
            .unwrap_or_default()
    }

    /// Set the spectate_position for the provided server.
    ///
    /// If the server does not exist then the appropriate fields
    /// will be initialised and the selected_character added.
    ///
    /// # Arguments
    ///
    /// * server - current server the player is on.
    /// * spectate_position - option containing the position we're spectating
    pub fn set_spectate_position(
        &mut self,
        server: &str,
        spectate_position: Option<vek::Vec3<f32>>,
    ) {
        self.servers
            .entry(server.to_string())
            .or_default()
            .spectate_position = spectate_position;
    }

    /// Save the current profile to disk.
    fn save_to_file(&self, config_dir: &Path) -> std::io::Result<()> {
        let path = Self::get_path(config_dir);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }

        let ron = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()).unwrap();
        fs::write(path, ron.as_bytes())
    }

    fn get_path(config_dir: &Path) -> PathBuf { config_dir.join("profile.ron") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_slots_with_empty_profile() {
        let profile = Profile::default();
        let slots = profile.get_hotbar_slots("TestServer", Some(CharacterId(12345)));
        assert_eq!(slots, [(); hud::HOTBAR_SLOT_COUNT].map(|()| None))
    }

    #[test]
    fn test_set_slots_with_empty_profile() {
        let mut profile = Profile::default();
        let slots = [(); hud::HOTBAR_SLOT_COUNT].map(|()| None);
        profile.set_hotbar_slots("TestServer", Some(CharacterId(12345)), slots);
    }

    #[test]
    fn mouse_slots_round_trip_independently_of_the_numbered_slots() {
        let mut profile = Profile::default();
        let mut slots = [(); hud::HOTBAR_SLOT_COUNT].map(|()| None);
        slots[0] = Some(hud::HotbarSlotContents::Ability(3));
        slots[10] = Some(hud::HotbarSlotContents::Ability(1)); // MouseLeft
        slots[11] = Some(hud::HotbarSlotContents::Ability(2)); // MouseRight

        profile.set_hotbar_slots("TestServer", Some(CharacterId(12345)), slots.clone());

        assert_eq!(
            profile.get_hotbar_slots("TestServer", Some(CharacterId(12345))),
            slots
        );
    }

    #[test]
    fn a_profile_ron_predating_the_mouse_slots_field_still_loads() {
        let ron = "(servers: {\"TestServer\": (characters: {12345: (hotbar_slots: (None, None, \
                   None, None, None, None, None, None, None, None))})})";
        let profile: Profile =
            ron::de::from_str(ron).expect("pre-mouse-slots profile.ron must still parse");
        assert_eq!(
            profile.get_hotbar_slots("TestServer", Some(CharacterId(12345))),
            [(); hud::HOTBAR_SLOT_COUNT].map(|()| None)
        );
    }
}
