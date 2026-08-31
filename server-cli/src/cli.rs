#![expect(
    clippy::needless_pass_by_ref_mut //until we find a better way for specs
)]

use chrono::{DateTime, Utc};
use clap::{Parser, builder::ValueParser};
use common::{comp, uuid::Uuid};
use server::persistence::SqlLogMode;
use std::{str::FromStr, sync::mpsc::Sender};
use tracing::error;

// Custom value parser for case-insensitive parsing of AdminRole
fn admin_role_value_parser() -> ValueParser {
    ValueParser::new(move |s: &str| -> Result<comp::AdminRole, String> {
        comp::AdminRole::from_str(&s.to_lowercase()).map_err(|err| err.to_string())
    })
}

#[derive(Clone, Debug, Parser)]
pub enum Admin {
    /// Adds an admin
    Add {
        /// Name of the admin to whom to assign a role
        username: String,
        /// Role to assign to the admin
        #[arg(value_parser = admin_role_value_parser())]
        role: comp::AdminRole,
    },
    Remove {
        /// Name of the admin from whom to remove any existing roles
        username: String,
    },
}

#[derive(Clone, Debug, Parser)]
pub enum Shutdown {
    /// Closes the server immediately
    Immediate,
    /// Shuts down the server gracefully
    Graceful {
        /// Number of seconds to wait before shutting down
        seconds: u64,
        #[arg(short, long, default_value = "The server is shutting down")]
        /// Shutdown reason
        reason: String,
    },
    /// Cancel any pending graceful shutdown.
    Cancel,
}

#[derive(Clone, Debug, Parser)]
pub enum SharedCommand {
    /// Perform operations on the admin list
    Admin {
        #[command(subcommand)]
        command: Admin,
    },
}

/// Where an `OracleTrigger` should resolve its spawn origin.
#[derive(Clone, Debug, Parser)]
pub enum OracleTarget {
    /// Resolve to a currently-online player's position by alias. Fails
    /// (never silently falls back to the world origin) if no online player
    /// has this alias.
    Player { alias: String },
    /// Use these world-space coordinates verbatim.
    Coords { x: f32, y: f32, z: f32 },
}

#[derive(Debug, Clone, Parser)]
pub enum Message {
    #[command(flatten)]
    Shared(SharedCommand),
    /// Shut down the server (or cancel a shut down)
    Shutdown {
        #[command(subcommand)]
        command: Shutdown,
    },
    /// Loads up the chunks at map center and adds a entity that mimics a
    /// player to keep them from despawning
    #[cfg(feature = "worldgen")]
    LoadArea {
        /// View distance of the loaded area
        view_distance: u32,
    },
    /// Enable or disable sql logging
    SqlLogMode {
        #[arg(default_value_t, value_parser = clap::value_parser!(SqlLogMode))]
        mode: SqlLogMode,
    },
    /// Disconnects all connected clients
    DisconnectAllClients,
    /// Returns who's online right now and what they're doing (alias, uuid,
    /// position, active character) -- see `PlayerDto`.
    ListPlayers,
    ListLogs,
    /// sends a msg to everyone on the server
    SendGlobalMsg {
        msg: String,
    },
    /// Sends `msg` to exactly the players named by `target_uuids`, skipping
    /// (and reporting back) any uuid that isn't currently connected.
    /// `operator_uuid` is recorded for our own audit log only -- not
    /// role-checked, need not belong to a registered admin/moderator. See
    /// `MessageReturn::TargetedMsgSent`.
    SendTargetedMsg {
        target_uuids: Vec<String>,
        #[arg(long)]
        operator_uuid: String,
        #[arg(long)]
        msg: String,
    },
    /// Uptime-adjacent server status not already exported via `/metrics`:
    /// version, player count, pending-shutdown state.
    ServerInfo,
    /// Tail of the ORACLE chronicle log, oldest-first, capped to `limit`
    /// most-recent entries.
    ListChronicle {
        limit: usize,
    },
    /// Ids of every currently-loaded ORACLE `.dmevent`/`.entity_template`.
    OracleListEvents,
    /// Fire (or, with `dry_run`, preview) a currently-loaded ORACLE
    /// `DmEvent`. Gated by `crate::web::ui::api`'s HTTP route through
    /// `server::oracle::policy::gated_trigger_dm_event` -- rate limit,
    /// per-event cooldown, per-event spawn cap, and the
    /// `OracleEventsEnabled` kill switch all apply.
    OracleTrigger {
        event_id: String,
        #[command(subcommand)]
        target: OracleTarget,
        #[arg(long)]
        dry_run: bool,
        /// Raises the per-trigger spawn cap up to the sanitize-time
        /// ceiling. The caller is responsible for only setting this after
        /// its own step-up re-auth; this message does not gate who may set
        /// it.
        #[arg(long)]
        high_impact_override: bool,
    },
    /// Enables or disables the HTTP ORACLE-trigger kill switch
    /// (`server::oracle::policy::OracleEventsEnabled`). Distinct from, and
    /// must never be confused with, `common::resources::OracleLive`, which
    /// gates player spellcasting and this message never touches.
    OracleEventsEnabled {
        enabled: bool,
    },
    /// Lists the characters belonging to `uuid` — the in-process half of
    /// NH-79's character-data endpoint for `xindeler-web-landing`.
    /// Read-only; resolves each character's last-saved-waypoint location via
    /// the same `Server::parse_locations` the character-select screen
    /// already uses. `uuid` may belong to a player who isn't currently
    /// connected — this reads persistence directly, not live ECS state.
    ListPlayerCharacters {
        uuid: String,
    },
    /// Renames one of `uuid`'s own characters — the write half of NH-79.
    /// Rejects if `character_id` doesn't belong to `uuid`, if `new_alias`
    /// fails `common::character::validate_character_name`, or if the name is
    /// already taken by any character (own namespace, never checked against
    /// account usernames — spec §4.4 of the NH-79 design doc).
    RenameCharacter {
        uuid: String,
        character_id: i64,
        new_alias: String,
    },
    /// Kicks `target_uuid`'s live session on behalf of `operator_uuid`, a
    /// registered admin/moderator with no live in-game session of its own.
    /// Fails if the operator isn't a real admin/moderator, if the target
    /// isn't currently connected, or if the operator's role doesn't outrank
    /// the target's.
    AdminKickPlayer {
        target_uuid: String,
        #[arg(long)]
        operator_uuid: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Bans `target_uuid` on behalf of `operator_uuid`, a registered
    /// admin/moderator with no live in-game session of its own.
    /// `duration_secs` of `None` means a permanent ban. `target_username`,
    /// if the caller has it, avoids a network round-trip to resolve it (see
    /// `server::cmd::admin_ban_player`'s doc comment) -- purely
    /// informational, falls back to the bare uuid if omitted.
    AdminBanPlayer {
        target_uuid: String,
        #[arg(long)]
        operator_uuid: String,
        #[arg(long)]
        target_username: Option<String>,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        duration_secs: Option<u64>,
        #[arg(long)]
        overwrite: bool,
    },
    /// Unbans `target_uuid` on behalf of `operator_uuid`, a registered
    /// admin/moderator with no live in-game session of its own.
    AdminUnbanPlayer {
        target_uuid: String,
        #[arg(long)]
        operator_uuid: String,
        #[arg(long)]
        target_username: Option<String>,
    },
    /// Lists the characters belonging to an arbitrary `uuid`, for a trusted
    /// admin caller. Unlike `ListPlayerCharacters` (whose HTTP route
    /// restricts it to the caller's own uuid), this variant is reachable
    /// with any uuid by design — it is only ever wired to an
    /// admin-secret-authenticated route.
    AdminListPlayerCharacters {
        uuid: String,
    },
    /// Suspends `character_id` on behalf of `operator_uuid`, a registered
    /// admin/moderator with no live in-game session of its own. Unlike
    /// `AdminBanPlayer` (account-wide), this freezes one character only.
    /// `target_uuid` must be the account that actually owns `character_id` --
    /// this refuses the action rather than silently acting on whatever
    /// account the character really belongs to, since a caller with a stale
    /// or mismatched `target_uuid` almost certainly has the wrong page open,
    /// not a deliberate cross-account request. `duration_secs` is required --
    /// `0` means permanent (i.e. lasts until a manual unsuspend); there is
    /// deliberately no way to omit it and get a permanent suspension by
    /// accident.
    AdminSuspendCharacter {
        target_uuid: String,
        character_id: i64,
        #[arg(long)]
        operator_uuid: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        duration_secs: u64,
    },
    /// Lifts a suspension on `character_id` on behalf of `operator_uuid`, a
    /// registered admin/moderator with no live in-game session of its own.
    /// Same `target_uuid`-must-own-`character_id` check as
    /// `AdminSuspendCharacter`.
    AdminUnsuspendCharacter {
        target_uuid: String,
        character_id: i64,
        #[arg(long)]
        operator_uuid: String,
    },
}

/// Ids of every currently-loaded ORACLE asset. See `Message::OracleListEvents`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OracleEventsDto {
    pub dm_events: Vec<String>,
    pub entity_templates: Vec<String>,
}

/// Server status for ops consumers that can't scrape Prometheus (e.g. an
/// admin console talking through `xindeler-zuul`). Some fields (`entity_count`,
/// `tick_time_ms`) mirror gauges already on `/metrics`; others
/// (`shutdown_pending_secs`, `uptime_secs`, `shutdown_reason`) exist only
/// here. See `Message::ServerInfo`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerInfoDto {
    pub version: String,
    pub player_count: usize,
    /// `Some(seconds)` while a graceful shutdown countdown is in progress
    /// (see `ShutdownCoordinator`); `None` otherwise.
    pub shutdown_pending_secs: Option<u64>,
    /// Number of ECS entities currently active on the server. Mirrors the
    /// `entity_count` Prometheus gauge (`TickMetrics::entity_count`).
    pub entity_count: usize,
    /// Wall-clock duration of the most recently completed tick, in
    /// milliseconds.
    pub tick_time_ms: u64,
    /// Seconds since this server process started.
    pub uptime_secs: u64,
    /// The message a pending graceful shutdown was initiated with, or `None`
    /// if no shutdown is currently in progress.
    pub shutdown_reason: Option<String>,
}

/// A character's resolved location. Only `site` resolves against real data
/// today — `kingdom`/`continent` are always `None` until a real hierarchy
/// exists (NH-79 spec §2.3/§2.4/§4.3); this is a contract decision made
/// ahead of that world-design work landing, not a per-character gap.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocationDto {
    pub site: String,
    pub kingdom: Option<String>,
    pub continent: Option<String>,
}

/// See `Message::ListPlayers`. Deliberately live-ECS-only, not
/// persistence-backed -- this route answers "who's online and what are they
/// doing right now", the complement of `AdminListPlayerCharacters`'s
/// persistence-backed "which characters does this account have" (works
/// offline, doesn't know what's active). A future account-listing source
/// (online or not) is expected to live elsewhere; this DTO never grows an
/// offline-accounts case.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlayerDto {
    pub alias: String,
    pub uuid: Uuid,
    /// `None` if the player has no `Pos` component (should not normally
    /// happen for a connected player, but the join is defensive).
    pub position: Option<[f32; 3]>,
    /// The character currently being played, if any. `None` if the player
    /// is connected but not in a character (e.g. at character select).
    pub character_id: Option<i64>,
}

/// A character's current suspension, for ops tooling. See
/// `Message::AdminSuspendCharacter`/`AdminUnsuspendCharacter`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SuspensionDto {
    pub reason: String,
    pub suspended_by_operator_uuid: String,
    pub suspended_at: DateTime<Utc>,
    /// `None` means permanent (i.e. lasts until a manual unsuspend).
    pub end_date: Option<DateTime<Utc>>,
}

/// See `Message::ListPlayerCharacters`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CharacterSummaryDto {
    pub character_id: i64,
    pub name: String,
    pub level: u32,
    pub class: String,
    /// `None` if this character never sat at a waypoint.
    pub location: Option<LocationDto>,
    /// `Some(_)` if this character is currently suspended.
    pub suspended: Option<SuspensionDto>,
}

#[derive(Debug, Clone)]
pub enum MessageReturn {
    Players(Vec<PlayerDto>),
    Logs(Vec<String>),
    Info(ServerInfoDto),
    Chronicle(Vec<String>),
    OracleEvents(OracleEventsDto),
    /// A real (non-`dry_run`) `OracleTrigger` fired.
    OracleTriggered {
        event_id: String,
        at: [f32; 3],
        requested: usize,
        spawned: usize,
        clamped: bool,
    },
    /// A `dry_run: true` `OracleTrigger` resolved everything and passed
    /// every check, but emitted nothing.
    OraclePreview {
        event_id: String,
        at: [f32; 3],
        requested: usize,
        spawned: usize,
        clamped: bool,
        bodies: Vec<String>,
        distance_to_nearest_player: Option<f32>,
    },
    /// A fallible operation failed. Every `Message` arm that can fail must
    /// send this instead of silently dropping the response sender, or the
    /// caller only ever observes a request timeout with no explanation.
    Error(String),
    /// Response to `Message::ListPlayerCharacters`.
    PlayerCharacters(Vec<CharacterSummaryDto>),
    /// Response to a successful `Message::RenameCharacter`. Failure goes
    /// through `Error(String)` above, same as every other fallible arm.
    CharacterRenamed,
    /// Response to a successful `Message::AdminKickPlayer` /
    /// `Message::AdminUnbanPlayer` (`ban: None`) or `Message::AdminBanPlayer`
    /// (`ban: Some(..)`, the resulting ban record). Failure goes through
    /// `Error(String)` above, same as every other fallible arm.
    AdminActionOk {
        ban: Option<common_net::msg::server::BanInfo>,
    },
    /// Response to `Message::SendTargetedMsg`: which of the requested
    /// `target_uuids` were currently connected and got the message, and
    /// which weren't. Not a failure even if `not_found` is non-empty --
    /// partial delivery isn't an error, callers that care can inspect it.
    TargetedMsgSent {
        delivered_to: Vec<String>,
        not_found: Vec<String>,
    },
}

#[derive(Parser)]
#[command(
    name = "Veloren server TUI",
    version = common::util::DISPLAY_VERSION.as_str(),
    about = "The veloren server tui allows sending commands directly to the running server.",
    author = "The veloren devs <https://gitlab.com/veloren/veloren>",
)]
#[clap(no_binary_name = true)]
pub struct TuiApp {
    #[command(subcommand)]
    command: Message,
}

#[derive(Debug, Clone, Copy, Parser)]
pub struct BenchParams {
    /// View distance of the loaded area (in chunks)
    #[arg(long)]
    pub view_distance: u32,
    /// Duration to run after loading completes (in seconds).
    #[arg(long)]
    pub duration: u32,
}

#[derive(Parser)]
pub enum ArgvCommand {
    #[command(flatten)]
    Shared(SharedCommand),
    /// Load an area, run the server for some time, and then exit (useful for
    /// profiling).
    Bench(BenchParams),
}

#[derive(Parser)]
#[command(
    name = "Veloren server CLI",
    version = common::util::DISPLAY_VERSION.as_str(),
    about = "The veloren server cli provides an easy to use interface to start a veloren server.",
    author = "The veloren devs <https://gitlab.com/veloren/veloren>",
)]
pub struct ArgvApp {
    #[arg(long, short)]
    /// Enables the tui
    pub tui: bool,
    #[arg(long, short)]
    /// Doesn't listen on STDIN
    ///
    /// Useful if you want to send the server in background, and your kernels
    /// terminal driver will send SIGTTIN to it otherwise. <https://www.gnu.org/savannah-checkouts/gnu/bash/manual/bash.html#Redirections> and you dont want to use `stty -tostop`
    /// or `nohub` or `tmux` or `screen` or `<<< \"\\004\"` to the program.
    pub non_interactive: bool,
    #[arg(long)]
    /// Run without auth enabled
    pub no_auth: bool,
    #[arg(default_value_t, long, short, value_parser = clap::value_parser!(SqlLogMode))]
    /// Enables SQL logging
    pub sql_log_mode: SqlLogMode,
    #[command(subcommand)]
    pub command: Option<ArgvCommand>,
}

pub fn parse_command(input: &str, msg_s: &mut Sender<Message>) {
    match TuiApp::try_parse_from(shell_words::split(input).unwrap_or_default()) {
        Ok(message) => {
            msg_s
                .send(message.command)
                .unwrap_or_else(|e| error!(?e, "Failed to send CLI message"));
        },
        Err(e) => error!("{}", e),
    }
}
