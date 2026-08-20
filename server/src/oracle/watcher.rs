//! Filesystem watcher for ORACLE's `.dmevent.ron`/`.dmevent.json` and
//! `.entity_template.ron`/`.entity_template.json` files.
//!
//! These files are runtime-written operator/AI input living outside
//! `VELOREN_ASSETS`, not shipped assets — they are deliberately not routed
//! through `common-assets` (which is rooted at the asset tree and expects
//! build-time content, not something a tool drops in mid-session).
//!
//! The watcher only detects raw filesystem events on a background thread and
//! forwards the touched path over a channel. Parsing, sanitizing, and
//! updating the in-memory [`OracleEvents`] table all happen on
//! [`OracleWatcher::poll`], called once per server tick — so the anti-chaos
//! sanitize pass and any resulting state mutation stay on the main thread,
//! same as every other server resource.

use std::path::{Path, PathBuf};

use common_oracle::{DmEvent, EntityTemplate, parse_dm_event, parse_entity_template};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher, recommended_watcher};

/// Env var overriding the ORACLE event watch directory.
pub const ORACLE_EVENTS_DIR_ENV: &str = "XINDELER_ORACLE_EVENTS_DIR";

/// Default runtime directory ORACLE writes `.dmevent.ron`/`.dmevent.json`/
/// `.entity_template.ron`/`.entity_template.json` files into. Resolved
/// independently of `common_base::userdata_dir()`/`VELOREN_USERDATA` — a
/// deployment that relocates userdata via those still needs
/// `XINDELER_ORACLE_EVENTS_DIR` set explicitly to follow.
#[must_use]
pub fn default_events_dir() -> PathBuf {
    std::env::var_os(ORACLE_EVENTS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./userdata/oracle_events"))
}

/// One parsed+sanitized ingestion result for a watched path.
#[derive(Debug, Clone, PartialEq)]
pub enum OracleAsset {
    DmEvent(DmEvent),
    EntityTemplate(EntityTemplate),
}

/// In-memory table of currently-loaded ORACLE assets, keyed by their path.
/// A file's removal from disk removes its entry here too — deletion is the
/// retirement trigger.
#[derive(Default)]
pub struct OracleEvents {
    assets: hashbrown::HashMap<PathBuf, OracleAsset>,
}

impl OracleEvents {
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&OracleAsset> { self.assets.get(path) }

    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, &OracleAsset)> { self.assets.iter() }

    pub fn dm_events(&self) -> impl Iterator<Item = (&PathBuf, &DmEvent)> {
        self.assets.iter().filter_map(|(path, asset)| match asset {
            OracleAsset::DmEvent(event) => Some((path, event)),
            OracleAsset::EntityTemplate(_) => None,
        })
    }

    pub fn entity_templates(&self) -> impl Iterator<Item = (&PathBuf, &EntityTemplate)> {
        self.assets.iter().filter_map(|(path, asset)| match asset {
            OracleAsset::EntityTemplate(template) => Some((path, template)),
            OracleAsset::DmEvent(_) => None,
        })
    }

    /// Reads, parses (branching by extension), and sanitizes `path`,
    /// inserting the result. A file that fails to parse, or whose name
    /// matches neither known suffix, logs a `warn!` and is otherwise
    /// ignored — never a panic (ORACLE-authored files are untrusted input).
    fn ingest(&mut self, path: PathBuf) {
        match read_and_parse(&path) {
            Ok(Some(asset)) => {
                self.assets.insert(path, asset);
            },
            Ok(None) => {},
            Err(err) => {
                tracing::warn!(
                    "oracle: failed to load {} ({err}); ignoring (load failed, server keeps \
                     running)",
                    path.display()
                );
            },
        }
    }

    fn retire(&mut self, path: &Path) { self.assets.remove(path); }
}

fn read_and_parse(path: &Path) -> Result<Option<OracleAsset>, Box<dyn std::error::Error>> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let is_json = path.extension().and_then(|ext| ext.to_str()) == Some("json");

    if file_name.ends_with(".dmevent.ron") || file_name.ends_with(".dmevent.json") {
        let bytes = std::fs::read(path)?;
        Ok(Some(OracleAsset::DmEvent(parse_dm_event(&bytes, is_json)?)))
    } else if file_name.ends_with(".entity_template.ron")
        || file_name.ends_with(".entity_template.json")
    {
        let bytes = std::fs::read(path)?;
        Ok(Some(OracleAsset::EntityTemplate(parse_entity_template(
            &bytes, is_json,
        )?)))
    } else {
        Ok(None)
    }
}

/// A raw, unparsed filesystem change forwarded from the watcher thread.
enum RawEvent {
    Changed(PathBuf),
    Removed(PathBuf),
}

/// Watches [`default_events_dir`] (or its override) for ORACLE files. Owns
/// the background `notify` watcher (kept alive for as long as this resource
/// lives) and the in-memory [`OracleEvents`] table it feeds.
pub struct OracleWatcher {
    // Kept alive for the resource's lifetime; dropping it stops the watch.
    _watcher: RecommendedWatcher,
    change_rx: crossbeam_channel::Receiver<RawEvent>,
    events: OracleEvents,
}

impl OracleWatcher {
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                "oracle: could not create the events dir {} ({err}); the watcher will not \
                 activate until it exists",
                dir.display()
            );
        }

        let (change_tx, change_rx) = crossbeam_channel::unbounded();
        let mut watcher =
            recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    let kind = event.kind;
                    for path in event.paths {
                        let raw = match kind {
                            EventKind::Remove(_) => RawEvent::Removed(path),
                            EventKind::Create(_) | EventKind::Modify(_) => RawEvent::Changed(path),
                            _ => continue,
                        };
                        let _ = change_tx.send(raw);
                    }
                },
                Err(err) => tracing::warn!(?err, "oracle: watcher error"),
            })
            .expect("failed to construct the oracle events filesystem watcher");

        if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            tracing::warn!(
                "oracle: could not watch {} ({err}); dropped files will not be picked up",
                dir.display()
            );
        }

        Self {
            _watcher: watcher,
            change_rx,
            events: OracleEvents::default(),
        }
    }

    #[must_use]
    pub fn events(&self) -> &OracleEvents { &self.events }

    /// Drains every filesystem change queued since the last call, ingesting
    /// or retiring [`OracleEvents`] entries accordingly. Call once per server
    /// tick.
    pub fn poll(&mut self) {
        while let Ok(raw) = self.change_rx.try_recv() {
            match raw {
                RawEvent::Changed(path) => self.events.ingest(path),
                RawEvent::Removed(path) => self.events.retire(&path),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use common_oracle::{DimensionConfig, SpawningRules};

    use super::*;

    fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    /// macOS gotcha: `$TMPDIR` (what `tempfile` uses) resolves through a
    /// `/var` -> `/private/var` symlink; `notify`'s macOS FSEvents backend
    /// reports the CANONICAL (symlink-resolved) path. Canonicalize up front
    /// so watcher-reported paths match the ones this test constructs itself.
    fn canonical_tempdir() -> tempfile::TempDir { tempfile::tempdir().expect("tempdir") }

    #[test]
    fn dropped_dmevent_file_is_ingested_within_1s() {
        let dir = canonical_tempdir();
        let root = dir.path().canonicalize().expect("canonicalize tempdir");
        let mut watcher = OracleWatcher::new(&root);

        let event = DmEvent {
            dimension_config: DimensionConfig {
                seed_modifier: 7,
                biome_profile: "grey_forest".to_owned(),
            },
            spawning_rules: SpawningRules {
                entity_templates: vec!["dread_wolf".to_owned()],
                spawn_count: 3.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let ron_text = ron::ser::to_string(&event).expect("DmEvent serializes");
        let file_path = root.join("dropped.dmevent.ron");
        std::fs::write(&file_path, ron_text).expect("write fixture");

        let ingested = wait_until(
            || {
                watcher.poll();
                matches!(
                    watcher.events().get(&file_path),
                    Some(OracleAsset::DmEvent(_))
                )
            },
            Duration::from_secs(1),
        );
        assert!(ingested, "dropped .dmevent.ron was not ingested within 1s");

        let Some(OracleAsset::DmEvent(loaded)) = watcher.events().get(&file_path) else {
            panic!("expected a DmEvent entry");
        };
        assert_eq!(loaded.spawning_rules.entity_templates, vec![
            "dread_wolf".to_owned()
        ]);
    }

    #[test]
    fn deleting_the_file_retires_the_event() {
        let dir = canonical_tempdir();
        let root = dir.path().canonicalize().expect("canonicalize tempdir");
        let mut watcher = OracleWatcher::new(&root);

        let file_path = root.join("temp.dmevent.ron");
        std::fs::write(
            &file_path,
            ron::ser::to_string(&DmEvent::default()).unwrap(),
        )
        .expect("write fixture");

        let ingested = wait_until(
            || {
                watcher.poll();
                watcher.events().get(&file_path).is_some()
            },
            Duration::from_secs(1),
        );
        assert!(ingested, "fixture was not ingested before deletion");

        std::fs::remove_file(&file_path).expect("remove fixture");

        let retired = wait_until(
            || {
                watcher.poll();
                watcher.events().get(&file_path).is_none()
            },
            Duration::from_secs(1),
        );
        assert!(retired, "deleted file was not retired within 1s");
    }

    #[test]
    fn malformed_dropped_file_is_ignored_not_panicked() {
        let dir = canonical_tempdir();
        let root = dir.path().canonicalize().expect("canonicalize tempdir");
        let mut watcher = OracleWatcher::new(&root);

        let file_path = root.join("garbage.dmevent.ron");
        std::fs::write(&file_path, b"not valid ron {{{").expect("write fixture");

        // Give the watcher a chance to see and reject the file; it must
        // never appear as a loaded asset, and polling must never panic.
        wait_until(
            || {
                watcher.poll();
                false
            },
            Duration::from_millis(200),
        );
        assert!(watcher.events().get(&file_path).is_none());
    }

    #[test]
    fn unrelated_file_extension_is_ignored() {
        let dir = canonical_tempdir();
        let root = dir.path().canonicalize().expect("canonicalize tempdir");
        let mut watcher = OracleWatcher::new(&root);

        let file_path = root.join("notes.txt");
        std::fs::write(&file_path, b"just some notes").expect("write fixture");

        wait_until(
            || {
                watcher.poll();
                false
            },
            Duration::from_millis(200),
        );
        assert!(watcher.events().get(&file_path).is_none());
    }
}
