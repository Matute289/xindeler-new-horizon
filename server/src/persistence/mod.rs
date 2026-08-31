//! DB operations and schema migrations

// Touch this comment if changes only include .sql files and no .rs so that
// migration happens.
// nya~

pub(in crate::persistence) mod character;
pub(crate) use character::convert_waypoint_from_database_json as parse_waypoint;
pub mod character_loader;
pub mod character_updater;
pub(in crate::persistence) mod chronicle;
mod diesel_to_rusqlite;
pub mod error;
mod json_models;
mod models;
pub mod portrait;

use crate::persistence::character_updater::PetPersistenceData;
use chrono::{DateTime, Utc};
use common::{character::CharacterId, comp};
use hashbrown::HashMap;
use refinery::Report;
use rusqlite::{
    Connection, DropBehavior, OpenFlags,
    trace::{TraceEvent, TraceEventCodes},
};
use std::{
    collections::VecDeque,
    fs,
    ops::Deref,
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tracing::info;

/// A struct of the components that are persisted to the DB for each character
#[derive(Debug)]
pub struct PersistedComponents {
    pub body: comp::Body,
    pub hardcore: Option<comp::Hardcore>,
    pub character_class: comp::CharacterClass,
    pub stats: comp::Stats,
    pub skill_set: comp::SkillSet,
    pub inventory: comp::Inventory,
    pub waypoint: Option<comp::Waypoint>,
    pub pets: Vec<PetPersistenceData>,
    pub active_abilities: comp::ActiveAbilities,
    pub map_marker: Option<comp::MapMarker>,
    pub ethos: comp::Ethos,
    /// BL-31: background chosen at creation. Server-authoritative default is
    /// `Background(None)` ("Uncommitted", P0 §Q1) if the client doesn't send
    /// one.
    pub background: comp::Background,
    /// A Warlock's pact. Meaningless for other classes. Server-authoritative
    /// default is `Pact::default()` (`Bound`, no patron chosen) if the
    /// client doesn't send one.
    pub pact: comp::Pact,
    /// Reactive trigger slots. Loaded with an infinite in-game projection on
    /// every cooling slot; `state_ext` rebuilds the projection from the
    /// authoritative wall clock before inserting the component.
    pub trigger_slots: comp::TriggerSlots,
    /// Per-`MagicSource` mastery progress. No load-time transform needed --
    /// unlike `trigger_slots` it carries no wall-clock state.
    pub spell_mastery: comp::SpellMastery,
}

pub type EditableComponents = (comp::Body,);

// See: https://docs.rs/refinery/0.5.0/refinery/macro.embed_migrations.html
// This macro is called at build-time, and produces the necessary migration info
// for the `run_migrations` call below.
mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("./src/migrations");
}

/// A database connection blessed by Veloren.
pub(crate) struct VelorenConnection {
    connection: Connection,
    sql_log_mode: SqlLogMode,
}

impl VelorenConnection {
    fn new(connection: Connection) -> Self {
        Self {
            connection,
            sql_log_mode: SqlLogMode::Disabled,
        }
    }

    /// Updates the SQLite log mode if DatabaseSetting.sql_log_mode has changed
    pub fn update_log_mode(&mut self, database_settings: &Arc<RwLock<DatabaseSettings>>) {
        let settings = database_settings
            .read()
            .expect("DatabaseSettings RwLock was poisoned");
        if self.sql_log_mode == settings.sql_log_mode {
            return;
        }

        set_log_mode(&mut self.connection, settings.sql_log_mode);
        self.sql_log_mode = settings.sql_log_mode;

        info!(
            "SQL log mode for connection changed to {:?}",
            settings.sql_log_mode
        );
    }
}

impl Deref for VelorenConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection { &self.connection }
}

fn set_log_mode(connection: &mut Connection, sql_log_mode: SqlLogMode) {
    match sql_log_mode {
        SqlLogMode::Trace => {
            connection.trace_v2(
                TraceEventCodes::SQLITE_TRACE_STMT,
                Some(rusqlite_trace_callback),
            );
        },
        SqlLogMode::Profile => {
            connection.trace_v2(
                TraceEventCodes::SQLITE_TRACE_PROFILE,
                Some(rusqlite_trace_callback),
            );
        },
        SqlLogMode::Disabled => {
            connection.trace_v2(TraceEventCodes::empty(), None);
        },
    };
}

#[derive(Clone)]
pub struct DatabaseSettings {
    pub db_dir: PathBuf,
    pub sql_log_mode: SqlLogMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SqlLogMode {
    /// Logging is disabled
    #[default]
    Disabled,
    /// Records timings for each SQL statement
    Profile,
    /// Prints all executed SQL statements
    Trace,
}

impl SqlLogMode {
    pub fn variants() -> [&'static str; 3] { ["disabled", "profile", "trace"] }
}

impl core::str::FromStr for SqlLogMode {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "disabled" => Ok(Self::Disabled),
            "profile" => Ok(Self::Profile),
            "trace" => Ok(Self::Trace),
            _ => Err("Could not parse SqlLogMode"),
        }
    }
}

#[expect(clippy::to_string_trait_impl)]
impl ToString for SqlLogMode {
    fn to_string(&self) -> String {
        match self {
            SqlLogMode::Disabled => "disabled",
            SqlLogMode::Profile => "profile",
            SqlLogMode::Trace => "trace",
        }
        .into()
    }
}

/// Runs any pending database migrations. This is executed during server startup
pub fn run_migrations(settings: &DatabaseSettings) {
    let mut conn = establish_connection(settings, ConnectionMode::ReadWrite);

    diesel_to_rusqlite::migrate_from_diesel(&mut conn)
        .expect("One-time migration from Diesel to Refinery failed");

    // If migrations fail to run, the server cannot start since the database will
    // not be in the required state.
    let report: Report = embedded::migrations::runner()
        .set_abort_divergent(false)
        .run(&mut conn.connection)
        .expect("Database migrations failed, server startup aborted");

    let applied_migrations = report.applied_migrations().len();
    info!("Applied {} database migrations", applied_migrations);
}

/// Runs after the migrations. In some cases, it can reclaim a significant
/// amount of space (reported 30%)
pub fn vacuum_database(settings: &DatabaseSettings) {
    let conn = establish_connection(settings, ConnectionMode::ReadWrite);

    conn.execute("VACUUM main", [])
        .expect("Database vacuuming failed, server startup aborted");

    info!("Database vacuumed");
}

/// The character data needed for NH-79's character-summary endpoint: alias,
/// class, computed level, and the raw saved waypoint. Deliberately lighter
/// than `common::character::CharacterItem` (used by `load_character_list`
/// for the character-select screen) -- no body/inventory/pets, none of which
/// a summary list needs. `waypoint` is the raw saved-waypoint string;
/// `Server::list_player_characters` resolves it to a site name via
/// `Server::parse_locations`, which needs world/index access persistence
/// doesn't have.
pub struct CharacterSummary {
    pub character_id: CharacterId,
    pub alias: String,
    pub class: String,
    pub level: u16,
    pub waypoint: Option<String>,
    pub suspended: Option<SuspensionRecord>,
}

/// A character's suspension, as stored in the `character_suspensions` table
/// and cached in the server's in-memory `CharacterSuspensions` resource.
/// `end_date` of `None` means permanent (i.e. lasts until a manual
/// unsuspend) -- never "the admin forgot to set one", since the admin
/// surface requires an explicit `duration_secs` up front (`0` spells out
/// permanent).
#[derive(Debug, Clone)]
pub struct SuspensionRecord {
    pub reason: String,
    pub suspended_by_operator_uuid: String,
    pub suspended_at: DateTime<Utc>,
    pub end_date: Option<DateTime<Utc>>,
}

/// NH-79: `xindeler-web-landing`'s player-characters endpoint reads
/// persistence for a uuid that may not even be connected right now, so
/// there's no live ECS state to read the way `CharacterLoader` serves
/// currently-loaded characters. Opens its own short-lived, read-only
/// connection using the same settings `CharacterLoader`'s background thread
/// already opens from.
pub fn list_player_characters(
    uuid: &str,
    settings: &DatabaseSettings,
) -> Result<Vec<CharacterSummary>, error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    character::load_character_summaries(uuid, &connection)
}

/// NH-79: the write half of `list_player_characters`. Opens its own
/// short-lived, read-write connection and transaction -- unreachable through
/// `CharacterUpdater`'s background channel since the target player may not be
/// connected right now.
pub fn rename_character(
    uuid: &str,
    character_id: CharacterId,
    new_alias: &str,
    settings: &DatabaseSettings,
) -> Result<(), error::PersistenceError> {
    let mut connection = establish_connection(settings, ConnectionMode::ReadWrite);
    let mut transaction = connection.connection.transaction()?;
    transaction.set_drop_behavior(DropBehavior::Rollback);
    character::rename_character(uuid, character_id, new_alias, &mut transaction)?;
    transaction.commit()?;
    Ok(())
}

/// The body and inventory a portrait for one of `uuid`'s own characters is
/// rendered from. Opens its own short-lived, read-only connection, for the
/// same reason `list_player_characters` does: the player whose portrait is
/// being fetched from the web may not be connected to the game at all, so
/// there is no live ECS state to read.
///
/// Ownership is enforced in the query itself; a character that isn't `uuid`'s
/// comes back as `CharacterNotFound`, exactly as a character that doesn't
/// exist does.
pub fn load_portrait_inputs(
    uuid: &str,
    character_id: CharacterId,
    settings: &DatabaseSettings,
) -> Result<(comp::Body, comp::Inventory), error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    portrait::load_portrait_inputs(character_id, uuid, &connection)
}

/// The cached portrait for `character_id`, if one has ever been rendered.
///
/// Deliberately a separate connection from `load_portrait_inputs` rather than
/// one combined read: the common steady-state request answers `304 Not
/// Modified` off the appearance key alone, and that path must not pay to pull
/// a blob it is never going to send.
///
/// Takes no `uuid`: the caller reaches a `character_id` only by having already
/// loaded it through `load_portrait_inputs`, which is where ownership is
/// decided.
pub fn get_portrait(
    character_id: CharacterId,
    settings: &DatabaseSettings,
) -> Result<Option<portrait::PortraitRow>, error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    portrait::get_portrait(character_id, &connection)
}

/// Caches a freshly rendered portrait, replacing any previous one. Opens its
/// own short-lived, read-write connection and transaction -- like
/// `rename_character`, this write is unreachable through `CharacterUpdater`'s
/// background channel, since the character's owner may not be connected.
///
/// There is deliberately no matching `delete_portrait` wrapper here: the only
/// thing that ever needs to evict a row is `delete_character`, which calls
/// `portrait::delete_portrait` inside its own transaction so the eviction is
/// atomic with the deletion. A second connection could only make that weaker.
pub fn upsert_portrait(
    character_id: CharacterId,
    appearance_key: &str,
    format: &str,
    image: &[u8],
    rendered_at: DateTime<Utc>,
    settings: &DatabaseSettings,
) -> Result<(), error::PersistenceError> {
    let mut connection = establish_connection(settings, ConnectionMode::ReadWrite);
    let mut transaction = connection.connection.transaction()?;
    transaction.set_drop_behavior(DropBehavior::Rollback);
    portrait::upsert_portrait(
        character_id,
        appearance_key,
        format,
        image,
        rendered_at,
        &mut transaction,
    )?;
    transaction.commit()?;
    Ok(())
}

/// Test-only: creates a character owned by `uuid` through the real
/// `create_character` path and returns its id.
///
/// Exists so that tests living outside `crate::persistence` -- the portrait
/// service's, at the crate root -- can build a genuine fixture (character row,
/// body row, five pseudo-containers, persisted loadout) instead of hand-writing
/// rows that would drift from the real schema, without `persistence::character`
/// having to become public for their sake.
#[cfg(test)]
pub(crate) fn create_character_for_test(
    uuid: &str,
    alias: &str,
    components: PersistedComponents,
    settings: &DatabaseSettings,
) -> Result<CharacterId, error::PersistenceError> {
    let mut connection = establish_connection(settings, ConnectionMode::ReadWrite);
    let mut transaction = connection.connection.transaction()?;
    transaction.set_drop_behavior(DropBehavior::Rollback);
    let (character_id, _) = character::create_character(uuid, alias, components, &mut transaction)?;
    transaction.commit()?;
    Ok(character_id)
}

/// Suspends `character_id`, replacing any existing suspension on it (a
/// re-suspend overwrites, it does not stack). Opens its own short-lived,
/// read-write connection -- unreachable through `CharacterUpdater`'s
/// background channel since the target character's owning player may not be
/// connected right now.
pub fn suspend_character(
    character_id: CharacterId,
    reason: &str,
    suspended_by_operator_uuid: &str,
    suspended_at: DateTime<Utc>,
    end_date: Option<DateTime<Utc>>,
    settings: &DatabaseSettings,
) -> Result<(), error::PersistenceError> {
    let mut connection = establish_connection(settings, ConnectionMode::ReadWrite);
    let mut transaction = connection.connection.transaction()?;
    transaction.set_drop_behavior(DropBehavior::Rollback);
    character::suspend_character(
        character_id,
        reason,
        suspended_by_operator_uuid,
        suspended_at,
        end_date,
        &mut transaction,
    )?;
    transaction.commit()?;
    Ok(())
}

/// Lifts any suspension on `character_id`. Idempotent -- deleting a row that
/// doesn't exist is not an error, same as `Banlist::ban_operation`'s unban
/// side (except that one distinguishes "no effect" for the caller; this one
/// doesn't need to, since re-unsuspending an already-clear character has no
/// meaningfully different outcome to report).
pub fn unsuspend_character(
    character_id: CharacterId,
    settings: &DatabaseSettings,
) -> Result<(), error::PersistenceError> {
    let mut connection = establish_connection(settings, ConnectionMode::ReadWrite);
    let mut transaction = connection.connection.transaction()?;
    transaction.set_drop_behavior(DropBehavior::Rollback);
    character::unsuspend_character(character_id, &mut transaction)?;
    transaction.commit()?;
    Ok(())
}

/// The current suspension row for `character_id`, if any, read straight from
/// the database rather than the in-memory cache. The server's live
/// enforcement path (character selection) never calls this -- it reads the
/// in-memory `CharacterSuspensions` resource instead, since a DB round-trip
/// on every character-select attempt would be needless latency for something
/// already cached. This exists for callers without ECS access, or that want
/// a ground-truth read independent of the cache.
pub fn get_character_suspension(
    character_id: CharacterId,
    settings: &DatabaseSettings,
) -> Result<Option<SuspensionRecord>, error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    character::get_character_suspension(character_id, &connection)
}

/// Every currently-suspended character, for `CharacterSuspensions::load` at
/// server startup -- the one time the in-memory cache is built from the
/// database rather than kept in sync incrementally.
pub fn load_all_character_suspensions(
    settings: &DatabaseSettings,
) -> Result<HashMap<CharacterId, SuspensionRecord>, error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    character::load_all_character_suspensions(&connection)
}

/// Resolves the account uuid that owns `character_id`. Suspension is
/// character-scoped, but `role_outranks` (the same operator-must-outrank-
/// target check `admin_ban_player`/`admin_unban_player` already apply) is
/// still an account-level concept, so the admin command needs this lookup to
/// find the account behind a bare character id.
pub fn character_owner_uuid(
    character_id: CharacterId,
    settings: &DatabaseSettings,
) -> Result<Option<String>, error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    character::character_owner_uuid(character_id, &connection)
}

/// Persists one ORACLE chronicle entry, pruning the table back to
/// [`crate::oracle::chronicle::bounds::MAX_ENTRIES`] in the same transaction
/// so the durable window always mirrors the in-memory one.
///
/// Opens its own short-lived, read-write connection rather than going through
/// `CharacterUpdater`'s background channel: that channel is sized for
/// frequent, batched, per-character writes keyed to entities, which is the
/// wrong shape for a rare, single-string, no-response-needed append. This
/// blocks on disk I/O, so it must only ever be called off the ECS tick thread
/// -- in the server that means from inside the "CHRONICLE_LOG" slow-job.
pub fn append_chronicle_entry(
    text: &str,
    created_at: DateTime<Utc>,
    settings: &DatabaseSettings,
) -> Result<(), error::PersistenceError> {
    let mut connection = establish_connection(settings, ConnectionMode::ReadWrite);
    let mut transaction = connection.connection.transaction()?;
    transaction.set_drop_behavior(DropBehavior::Rollback);
    chronicle::insert_and_prune_chronicle_entry(
        text,
        created_at,
        crate::oracle::chronicle::bounds::MAX_ENTRIES,
        &mut transaction,
    )?;
    transaction.commit()?;
    Ok(())
}

/// The persisted chronicle entries, oldest-first, for `ChronicleLog::load` at
/// server startup -- the one time the in-memory log is built from the database
/// rather than appended to incrementally.
pub fn load_chronicle_log_tail(
    settings: &DatabaseSettings,
) -> Result<VecDeque<String>, error::PersistenceError> {
    let connection = establish_connection(settings, ConnectionMode::ReadOnly);
    chronicle::load_chronicle_tail(crate::oracle::chronicle::bounds::MAX_ENTRIES, &connection)
}

// This callback uses info logging because it is never enabled by default,
// only when explicitly turned on via CLI arguments or interactive CLI commands.
// Setting it to anything other than info would remove the ability to get SQL
// logging from a running server that wasn't started at higher than info.
fn rusqlite_trace_callback(event: TraceEvent<'_>) {
    match event {
        TraceEvent::Stmt(_, msg) => info!("{}", msg),
        TraceEvent::Profile(stmt, dur) => info!("{} Duration: {:?}", stmt.sql(), dur),
        _ => (),
    }
}

pub(crate) fn establish_connection(
    settings: &DatabaseSettings,
    connection_mode: ConnectionMode,
) -> VelorenConnection {
    fs::create_dir_all(&settings.db_dir)
        .unwrap_or_else(|_| panic!("Failed to create saves directory: {:?}", settings.db_dir));

    let open_flags = OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | match connection_mode {
            ConnectionMode::ReadWrite => {
                OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE
            },
            ConnectionMode::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY,
        };

    let connection = Connection::open_with_flags(settings.db_dir.join("db.sqlite"), open_flags)
        .unwrap_or_else(|err| {
            panic!(
                "Error connecting to {}, Error: {:?}",
                settings.db_dir.join("db.sqlite").display(),
                err
            )
        });

    let mut veloren_connection = VelorenConnection::new(connection);

    let connection = &mut veloren_connection.connection;

    set_log_mode(connection, settings.sql_log_mode);
    veloren_connection.sql_log_mode = settings.sql_log_mode;

    rusqlite::vtab::array::load_module(connection).expect("Failed to load sqlite array module");

    connection.set_prepared_statement_cache_capacity(100);

    // Use Write-Ahead-Logging for improved concurrency: https://sqlite.org/wal.html
    // Set a busy timeout (in ms): https://sqlite.org/c3ref/busy_timeout.html
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("Failed to set foreign_keys PRAGMA");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("Failed to set journal_mode PRAGMA");
    connection
        .pragma_update(None, "busy_timeout", "250")
        .expect("Failed to set busy_timeout PRAGMA");

    veloren_connection
}
