//! Character portraits: the appearance key that decides when one is stale, and
//! the worker that renders one when it is.
//!
//! A portrait is an image of a character's persisted body wearing its
//! persisted loadout, served to the web account page. It is drawn by
//! `portrait_gen`, a short-lived headless subprocess in the voxygen crate --
//! not in this process, and not on the game tick. Nothing here touches the ECS,
//! and the only thing the main loop ever does for a portrait is hand
//! [`PortraitServiceHandle::request`] a message and move on.
//!
//! # How staleness is decided
//!
//! There is no dirty flag anywhere in the equip, save or login paths. Instead
//! every request rebuilds an [`appearance_key`] from what the database
//! currently holds and compares it with the key the cached image was rendered
//! from. Equal means the cache is good; different, or absent, means render.
//! That is the whole invalidation mechanism, and it costs the rest of the
//! server exactly nothing.
//!
//! # Talking to `portrait_gen`
//!
//! The two binaries share no code -- pulling the renderer into a crate the
//! server could link would drag voxygen's dependency graph into it -- so they
//! agree by a wire format instead: a JSON request on stdin, encoded image bytes
//! on stdout, and a [`PORTRAIT_PARAMS_VERSION`] the renderer refuses to
//! disagree about. The constants below are duplicated from that binary on
//! purpose, and pinned by `default_params_serialize_to_the_pinned_shape`, which
//! mirrors the identically-named test on its side: if either end changes the
//! shape, both tests fail rather than the two silently caching each other's
//! output under a key that claims otherwise.

use crate::persistence::{self, DatabaseSettings, error::PersistenceError};
use chrono::Utc;
use common::{
    character::CharacterId,
    comp::{
        Body, Inventory,
        inventory::item::ItemDefinitionId,
        slot::{ArmorSlot, EquipSlot},
    },
};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tracing::{debug, error, warn};

// ---------------------------------------------------------------------------
// The half of `portrait_gen`'s wire format this side has to reproduce
// ---------------------------------------------------------------------------

/// Renderer/params version. Must equal `portrait_gen`'s constant of the same
/// name: the renderer rejects a request that asks for any other version.
///
/// It is also the prefix of every [`appearance_key`], which is what makes
/// bumping it invalidate every cached portrait lazily, with no migration and no
/// sweep -- the next request for each character simply finds a key that no
/// longer matches. Bump it for *any* change to what a portrait looks like,
/// including [`DEFAULT_PORTRAIT_SIZE`] and the framing preset, neither of which
/// is otherwise part of the key.
pub const PORTRAIT_PARAMS_VERSION: &str = "p1";

/// Edge length, in pixels, of the square portrait this server asks for.
pub const DEFAULT_PORTRAIT_SIZE: u16 = 256;

/// Framing preset name, as `portrait_gen` serializes its `Framing` enum.
const PORTRAIT_FRAMING: &str = "full_body_front";

/// Subtype of the `image/*` media type `portrait_gen` writes, stored in the
/// cache row's `format` column and handed to the HTTP layer as-is.
pub const PORTRAIT_FORMAT: &str = "webp";

/// Ceiling on what a render may write to stdout. An encoded portrait is tens of
/// kilobytes; this exists so a renderer that goes wrong costs one buffered
/// megabyte rather than the server's memory.
const MAX_IMAGE_BYTES: usize = 1024 * 1024;

/// How long a render gets before it is killed. Generous next to the tens of
/// milliseconds a warm render measures at, because the first render after a
/// restart also pays for loading the humanoid manifests off a cold page cache.
const RENDER_TIMEOUT: Duration = Duration::from_secs(15);

/// Ceiling on how much of the renderer's stderr is kept. It writes one
/// diagnostic line; this is only here so that a renderer gone wrong cannot log
/// without bound.
const MAX_STDERR_BYTES: usize = 4096;

/// How long the worker waits for an already-exited renderer's pipes to reach
/// EOF before giving up on them. Not a work timeout -- see `collect`.
const PIPE_COLLECT_GRACE: Duration = Duration::from_secs(1);

/// How often the worker checks on a running render. Only ever reached by a
/// render that is still going, so the cost is a handful of wakeups per render.
const RENDER_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Depth of the request queue. Concurrency is one render at a time by
/// construction, so this is purely how much waiting is allowed before a caller
/// is told to come back later instead of being enqueued behind a minute of
/// work.
const QUEUE_DEPTH: usize = 4;

/// The `params` object of `portrait_gen`'s request, reproduced field for field.
///
/// Borrowed strings rather than owned ones: this is only ever serialized from
/// the constants above, never parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct PortraitParams {
    size: u16,
    framing: &'static str,
    version: &'static str,
}

impl Default for PortraitParams {
    fn default() -> Self {
        Self {
            size: DEFAULT_PORTRAIT_SIZE,
            framing: PORTRAIT_FRAMING,
            version: PORTRAIT_PARAMS_VERSION,
        }
    }
}

/// The whole of `portrait_gen`'s stdin.
#[derive(Debug, Serialize)]
struct PortraitRequest<'a> {
    body: &'a Body,
    inventory: &'a Inventory,
    params: PortraitParams,
}

// ---------------------------------------------------------------------------
// Appearance key
// ---------------------------------------------------------------------------

/// The equipment slots that change what a character looks like, in the order
/// they appear in an [`appearance_key`].
///
/// This is `CharacterCacheKey`'s slot list (voxygen's figure cache) restated
/// here rather than imported: the type lives behind voxygen's GPU-bound figure
/// module, and the server having a dependency on the client's renderer to
/// decide when a database row is stale would be a far worse coupling than one
/// list kept deliberately in step. If a slot is ever added there, add it here.
///
/// The two weapon slots are in the list even though the shipped framing preset
/// does not draw them. They belong to the *appearance*, and a preset that shows
/// them is an additive change to the renderer -- at which point every cached
/// portrait of an armed character must already be invalidated by the key it was
/// stored under, not by a migration written after the fact. Their cost while
/// the preset hides them is one needless re-render per weapon swap, which the
/// version prefix would have forced anyway.
///
/// The *inactive* weapon slots are deliberately absent, matching the figure
/// cache: a sheathed second loadout is not visible on the model.
const KEYED_SLOTS: &[(&str, EquipSlot)] = &[
    ("head", EquipSlot::Armor(ArmorSlot::Head)),
    ("shoulder", EquipSlot::Armor(ArmorSlot::Shoulders)),
    ("chest", EquipSlot::Armor(ArmorSlot::Chest)),
    ("belt", EquipSlot::Armor(ArmorSlot::Belt)),
    ("back", EquipSlot::Armor(ArmorSlot::Back)),
    ("pants", EquipSlot::Armor(ArmorSlot::Legs)),
    ("hand", EquipSlot::Armor(ArmorSlot::Hands)),
    ("foot", EquipSlot::Armor(ArmorSlot::Feet)),
    ("lantern", EquipSlot::Lantern),
    ("glider", EquipSlot::Glider),
    ("mainhand", EquipSlot::ActiveMainhand),
    ("offhand", EquipSlot::ActiveOffhand),
];

/// A canonical, versioned description of everything that decides what a
/// character looks like: the body it was created with, and what it is wearing.
///
/// The result is compared for equality and never parsed, so its only real
/// obligations are to be deterministic (two calls on the same character in the
/// same state must agree, whatever order that character's inventory happens to
/// be laid out in -- only `equipped` lookups are consulted, never slot order)
/// and injective enough that two different appearances cannot collide. It is
/// stored as-is rather than hashed so that a cache that misbehaves in
/// production can be diagnosed by reading the column; the short, opaque token
/// the HTTP layer needs is [`etag`] of this.
pub fn appearance_key(body: &Body, inventory: &Inventory) -> String {
    let mut key = String::with_capacity(256);
    key.push_str(PORTRAIT_PARAMS_VERSION);
    key.push_str("|body:");
    key.push_str(&body_key(body));

    for (name, slot) in KEYED_SLOTS {
        key.push('|');
        key.push_str(name);
        key.push(':');
        if let Some(item) = inventory.equipped(*slot) {
            key.push_str(&item_key(&item.item_definition_id()));
        }
    }

    key
}

/// The body half of an [`appearance_key`].
///
/// The humanoid fields are destructured exhaustively on purpose: adding a field
/// to `humanoid::Body` -- an upstream merge could -- then fails to compile here
/// instead of silently producing a key that ignores it, which would leave every
/// character wearing the new feature stuck on a stale portrait forever.
fn body_key(body: &Body) -> String {
    match body {
        Body::Humanoid(body) => {
            let common::comp::humanoid::Body {
                species,
                body_type,
                hair_style,
                beard,
                eyes,
                accessory,
                hair_color,
                skin,
                eye_color,
                height_scale,
            } = body;
            format!(
                "humanoid/{species:?}/{body_type:?}/{hair_style}/{beard}/{eyes}/{accessory}/\
                 {hair_color}/{skin}/{eye_color}/{height_scale}"
            )
        },
        // Not reachable for a player character, and `portrait_gen` refuses to
        // draw one anyway. Kept total rather than panicking: this function's
        // whole job is to decide whether a cache row is stale, and there is no
        // appearance for which "crash the worker thread" is the right answer.
        other => format!("other/{other:?}"),
    }
}

/// One equipped item, as the key sees it.
///
/// The full definition id including modular/compound components, where the
/// figure cache keeps only the pieces that pick a model. That is strictly more
/// sensitive than it needs to be -- swapping a component that does not change
/// the mesh forces one re-render -- and deliberately so: over-invalidating
/// costs a render nobody sees, while under-invalidating shows a player the
/// wrong armour and cannot be noticed from this side.
fn item_key(id: &ItemDefinitionId<'_>) -> String {
    match id {
        ItemDefinitionId::Simple(id) => id.to_string(),
        ItemDefinitionId::Modular {
            pseudo_base,
            components,
        }
        | ItemDefinitionId::Compound {
            simple_base: pseudo_base,
            components,
        } => {
            let components = components
                .iter()
                .map(item_key)
                .collect::<Vec<_>>()
                .join(",");
            format!("{pseudo_base}({components})")
        },
    }
}

/// The entity tag for an appearance: the SHA-256 of its key, hex encoded.
///
/// Returned bare, without the quotes an `ETag` header wants around it -- adding
/// them is the HTTP layer's job, and it is also the layer that has to strip
/// them off an incoming `If-None-Match`. See [`etag_matches`].
pub fn etag(appearance_key: &str) -> String {
    hex::encode(Sha256::digest(appearance_key.as_bytes()))
}

/// Whether a client-supplied `If-None-Match` value refers to `etag`.
///
/// Tolerates the two decorations the header may legitimately carry -- the
/// surrounding quotes, and a `W/` weak-comparison prefix -- because a portrait
/// is served whole or not at all, so weak and strong comparison mean the same
/// thing here. The value is compared and then discarded; it is never parsed
/// into anything, never stored, and never reaches a query.
pub fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    if_none_match.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        candidate.trim_matches('"') == etag
    })
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// What answering one portrait request came to. Maps one-to-one onto the HTTP
/// responses the route above this serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortraitOutcome {
    /// The portrait, either straight out of the cache or freshly rendered.
    Fresh {
        bytes: Vec<u8>,
        /// Subtype of the image's `image/*` media type.
        format: String,
        /// Bare, unquoted.
        etag: String,
    },
    /// The caller's `If-None-Match` already names the character's current
    /// appearance. No blob was read and nothing was rendered.
    NotModified { etag: String },
    /// The render queue is full. A retry later is the right response; the
    /// request was not attempted at all.
    Busy,
    /// No such character, or not this caller's. Deliberately one outcome for
    /// both, so the endpoint cannot be used to discover which character ids
    /// exist.
    NotFound,
    /// Something went wrong on this side. Already logged, with the character
    /// id; the caller gets no detail.
    Failed,
}

/// One request for the worker, carrying the channel its answer goes back on.
pub struct PortraitRequestMsg {
    /// The player the caller authenticated as. Ownership of `character_id` is
    /// checked against this in SQL, not here.
    pub uuid: String,
    pub character_id: CharacterId,
    /// The caller's `If-None-Match` header, if it sent one, exactly as
    /// received.
    pub if_none_match: Option<String>,
    /// Dropped without a send if the caller went away, which the worker treats
    /// as nothing worth reporting.
    pub respond: tokio::sync::oneshot::Sender<PortraitOutcome>,
}

/// Hands requests to the portrait worker. Cloneable and cheap; every clone
/// feeds the same single worker.
#[derive(Clone)]
pub struct PortraitServiceHandle {
    requests: Sender<PortraitRequestMsg>,
}

impl PortraitServiceHandle {
    /// Queues `msg`, or answers it immediately if the queue is full.
    ///
    /// Never blocks and never renders on the calling thread: this is called
    /// from the server's main loop, where a render would stall everything else
    /// the loop answers. A full queue is [`PortraitOutcome::Busy`] rather than
    /// a wait, so that a burst of requests degrades into "try again" instead of
    /// into an unbounded backlog of work whose callers have long since given
    /// up.
    pub fn request(&self, msg: PortraitRequestMsg) {
        match self.requests.try_send(msg) {
            Ok(()) => {},
            Err(TrySendError::Full(msg)) => {
                debug!(
                    character_id = ?msg.character_id,
                    "portrait queue is full, answering Busy"
                );
                let _ = msg.respond.send(PortraitOutcome::Busy);
            },
            Err(TrySendError::Disconnected(msg)) => {
                // The worker thread is gone, which it only is if it panicked.
                // Busy would invite a retry loop against something that is
                // never coming back.
                error!("the portrait worker is gone; portraits are unavailable");
                let _ = msg.respond.send(PortraitOutcome::Failed);
            },
        }
    }
}

/// The portrait worker: one thread, one render at a time, its own database
/// connections.
pub struct PortraitService {
    database_settings: Arc<RwLock<DatabaseSettings>>,
    portrait_gen_path: PathBuf,
    render_timeout: Duration,
}

impl PortraitService {
    /// Starts the worker and returns the handle the rest of the server talks
    /// to.
    ///
    /// `portrait_gen_path` is not required to exist: a server built without the
    /// renderer beside it is a perfectly normal development setup, and the
    /// failure belongs to the first request that actually needs a render (a
    /// logged `Failed`), not to server startup.
    pub fn spawn(
        database_settings: Arc<RwLock<DatabaseSettings>>,
        portrait_gen_path: PathBuf,
    ) -> PortraitServiceHandle {
        Self::spawn_with(
            database_settings,
            portrait_gen_path,
            RENDER_TIMEOUT,
            QUEUE_DEPTH,
        )
    }

    /// [`spawn`](Self::spawn) with the two bounds that are constants in
    /// production made explicit, so tests can exercise a timeout without
    /// waiting out the real one, and a full queue without filling four slots.
    fn spawn_with(
        database_settings: Arc<RwLock<DatabaseSettings>>,
        portrait_gen_path: PathBuf,
        render_timeout: Duration,
        queue_depth: usize,
    ) -> PortraitServiceHandle {
        let (requests, incoming) = bounded::<PortraitRequestMsg>(queue_depth);
        let service = Self {
            database_settings,
            portrait_gen_path,
            render_timeout,
        };

        std::thread::Builder::new()
            .name("portrait".to_owned())
            .spawn(move || {
                while let Ok(msg) = incoming.recv() {
                    let PortraitRequestMsg {
                        uuid,
                        character_id,
                        if_none_match,
                        respond,
                    } = msg;
                    let outcome = service.answer(&uuid, character_id, if_none_match.as_deref());
                    let _ = respond.send(outcome);
                }
            })
            .expect("failed to spawn the portrait worker thread");

        PortraitServiceHandle { requests }
    }

    /// Ownership check and load, then cache, then -- only if it has to --
    /// render.
    fn answer(
        &self,
        uuid: &str,
        character_id: CharacterId,
        if_none_match: Option<&str>,
    ) -> PortraitOutcome {
        let settings = self
            .database_settings
            .read()
            .expect("DatabaseSettings RwLock was poisoned")
            .clone();

        // Ownership first, before anything expensive and before anything is
        // revealed: this is the only step that distinguishes a character the
        // caller owns from one it does not.
        let (body, inventory) =
            match persistence::load_portrait_inputs(uuid, character_id, &settings) {
                Ok(inputs) => inputs,
                Err(PersistenceError::CharacterNotFound) => return PortraitOutcome::NotFound,
                Err(err) => {
                    error!(%err, ?character_id, "could not load a character's portrait inputs");
                    return PortraitOutcome::Failed;
                },
            };

        let appearance_key = appearance_key(&body, &inventory);
        let etag = etag(&appearance_key);

        // Answered before the cache is even consulted: a matching
        // `If-None-Match` says the caller already holds the image for the
        // appearance the database currently describes, which is true whatever
        // this server has cached.
        if if_none_match.is_some_and(|value| etag_matches(value, &etag)) {
            return PortraitOutcome::NotModified { etag };
        }

        match persistence::get_portrait(character_id, &settings) {
            Ok(Some(row)) if row.appearance_key == appearance_key => {
                return PortraitOutcome::Fresh {
                    bytes: row.image,
                    format: row.format,
                    etag,
                };
            },
            // Missing or stale: fall through and render.
            Ok(_) => {},
            Err(err) => {
                // A cache that cannot be read is a slow cache, not a broken
                // request.
                warn!(%err, ?character_id, "could not read the cached portrait; re-rendering");
            },
        }

        let bytes = match self.render(&body, &inventory) {
            Ok(bytes) => bytes,
            Err(err) => {
                error!(%err, ?character_id, "portrait render failed");
                return PortraitOutcome::Failed;
            },
        };

        // Only a successful render is cached, and a cache write that fails
        // still serves the image it could not store -- the next request simply
        // renders again.
        if let Err(err) = persistence::upsert_portrait(
            character_id,
            &appearance_key,
            PORTRAIT_FORMAT,
            &bytes,
            Utc::now(),
            &settings,
        ) {
            warn!(%err, ?character_id, "could not cache a rendered portrait");
        }

        PortraitOutcome::Fresh {
            bytes,
            format: PORTRAIT_FORMAT.to_owned(),
            etag,
        }
    }

    /// Runs one `portrait_gen` and returns what it drew.
    ///
    /// The command line is fixed: nothing a caller supplied ever becomes an
    /// argument, and the whole request travels as JSON on stdin.
    fn render(&self, body: &Body, inventory: &Inventory) -> Result<Vec<u8>, RenderError> {
        let request = serde_json::to_vec(&PortraitRequest {
            body,
            inventory,
            params: PortraitParams::default(),
        })
        .map_err(|err| RenderError::Request(err.to_string()))?;

        let mut command = nice_command(&self.portrait_gen_path);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| RenderError::Spawn(err.to_string()))?;

        // All three pipes are handled off this thread, so that the timeout
        // below is the *only* thing that decides how long a render may take.
        // Each of them can block for as long as the child feels like: a
        // request larger than a pipe buffer stalls the write until the child
        // reads, an image larger than one stalls the child until something
        // reads, and killing a child does not close a pipe that anything the
        // child spawned still holds open. Doing any of it here would be a wait
        // the timeout cannot interrupt.
        let mut stdin = child.stdin.take().expect("stdin was piped");
        std::thread::spawn(move || {
            // A write error is not reported: it means the renderer exited
            // early, and its exit code says why far more precisely than
            // `EPIPE` does. Dropping the pipe afterwards is what gives the
            // renderer its EOF.
            if let Err(err) = stdin.write_all(&request).and_then(|()| stdin.flush()) {
                debug!(%err, "the portrait renderer stopped reading its request");
            }
        });

        let stdout = child.stdout.take().expect("stdout was piped");
        let image = drain(move |buffer| {
            // One byte past the cap, so that hitting it is distinguishable
            // from an image that happens to be exactly that long.
            stdout
                .take(MAX_IMAGE_BYTES as u64 + 1)
                .read_to_end(buffer)
                .map(|_| ())
        });
        let stderr = child.stderr.take().expect("stderr was piped");
        let diagnostics = drain(move |buffer| {
            stderr
                .take(MAX_STDERR_BYTES as u64)
                .read_to_end(buffer)
                .map(|_| ())
        });

        let deadline = Instant::now() + self.render_timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                },
                Ok(None) => std::thread::sleep(RENDER_POLL_INTERVAL),
                Err(err) => return Err(RenderError::Spawn(err.to_string())),
            }
        };

        // Nothing the child wrote can matter once it has been killed, and
        // waiting for it might not terminate, so the timeout path collects
        // neither pipe. The helper threads are left to finish on their own:
        // each holds one bounded buffer and ends as soon as whatever still has
        // the other end of its pipe lets go.
        let Some(status) = status else {
            return Err(RenderError::TimedOut(self.render_timeout));
        };

        let stderr = collect(diagnostics)
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
            .unwrap_or_default();
        if !stderr.is_empty() {
            debug!(%stderr, "portrait renderer diagnostics");
        }

        let bytes = collect(image).map_err(RenderError::Output)?;

        // Anything outside the renderer's documented exit codes -- another
        // code, or termination by a signal -- means it crashed. Both workspace
        // profiles abort on panic, so a panic cannot arrive as a documented
        // failure code. Every one of these is terminal for this request; none
        // of them is retried.
        match status.code() {
            Some(0) => {},
            // 2 bad request, 3 unsupported body, 4 any other renderer-side
            // failure -- the renderer's whole documented failure range.
            Some(code @ 2..=4) => {
                return Err(RenderError::Refused {
                    code,
                    stderr: stderr.to_owned(),
                });
            },
            Some(code) => {
                return Err(RenderError::Crashed {
                    code: Some(code),
                    stderr: stderr.to_owned(),
                });
            },
            None => {
                return Err(RenderError::Crashed {
                    code: None,
                    stderr: stderr.to_owned(),
                });
            },
        }

        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(RenderError::TooLarge(bytes.len()));
        }
        if bytes.is_empty() {
            return Err(RenderError::Empty);
        }

        Ok(bytes)
    }
}

/// Reads one of the child's pipes on its own thread, handing the result back
/// over a channel rather than a `JoinHandle`.
///
/// A channel and not a join precisely because the caller must be able to give
/// up: a thread blocked on a pipe some orphaned grandchild still holds open
/// would otherwise take the worker down with it, which is the one thing the
/// render timeout exists to prevent. An abandoned drain thread costs one
/// bounded buffer and ends the moment the write end finally closes.
fn drain<F>(read: F) -> Receiver<std::io::Result<Vec<u8>>>
where
    F: FnOnce(&mut Vec<u8>) -> std::io::Result<()> + Send + 'static,
{
    let (done, result) = bounded(1);
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = done.send(read(&mut buffer).map(|()| buffer));
    });
    result
}

/// Takes what a [`drain`] thread read, once the child it was reading from has
/// already exited.
///
/// The grace period is for the handful of microseconds between a process
/// exiting and its pipes reaching EOF, not for waiting on work: by the time
/// this is called there is nothing left alive that should still be writing.
fn collect(result: Receiver<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>, String> {
    match result.recv_timeout(PIPE_COLLECT_GRACE) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(format!(
            "the renderer's output was still open {PIPE_COLLECT_GRACE:?} after it exited"
        )),
    }
}

/// `portrait_gen`, wrapped so it always loses a contended CPU to the game
/// server.
///
/// A render is a single-threaded CPU burn of a second or so on a box with two
/// shared cores, for a cosmetic image; the scheduler should prefer literally
/// anything the server itself is doing. `nice` is part of coreutils, so it is
/// present wherever this server runs; elsewhere there is no portable
/// equivalent and the renderer is spawned directly.
fn nice_command(portrait_gen_path: &Path) -> Command {
    if cfg!(unix) {
        let mut command = Command::new("nice");
        command.args(["-n", "19"]).arg(portrait_gen_path);
        command
    } else {
        Command::new(portrait_gen_path)
    }
}

/// Why a render produced no image. Logged here and never shown to a caller,
/// which only ever learns [`PortraitOutcome::Failed`].
#[derive(Debug)]
enum RenderError {
    Request(String),
    Spawn(String),
    Output(String),
    TimedOut(Duration),
    /// The renderer ran and refused the request, with one of its documented
    /// exit codes.
    Refused {
        code: i32,
        stderr: String,
    },
    /// The renderer did not survive: an undocumented exit code, or a signal.
    Crashed {
        code: Option<i32>,
        stderr: String,
    },
    TooLarge(usize),
    Empty,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(err) => write!(f, "could not serialize the render request: {err}"),
            Self::Spawn(err) => write!(f, "could not run the portrait renderer: {err}"),
            Self::Output(err) => write!(f, "could not read the portrait renderer's output: {err}"),
            Self::TimedOut(after) => write!(f, "the portrait renderer was killed after {after:?}"),
            Self::Refused { code, stderr } => {
                write!(
                    f,
                    "the portrait renderer refused the request ({code}): {stderr}"
                )
            },
            Self::Crashed {
                code: Some(code),
                stderr,
            } => {
                write!(
                    f,
                    "the portrait renderer exited with an undocumented code {code}: {stderr}"
                )
            },
            Self::Crashed { code: None, stderr } => {
                write!(
                    f,
                    "the portrait renderer was terminated by a signal: {stderr}"
                )
            },
            Self::TooLarge(len) => write!(
                f,
                "the portrait renderer wrote {len} bytes, over the {MAX_IMAGE_BYTES} byte cap"
            ),
            Self::Empty => write!(f, "the portrait renderer wrote no image"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        comp::{self, humanoid},
        resources::Time,
    };

    const CHEST: &str = "common.items.armor.cloth_blue.chest";
    const OTHER_CHEST: &str = "common.items.armor.cloth_green.chest";

    fn body() -> Body { Body::Humanoid(humanoid::Body::random()) }

    fn wearing(slots: &[(EquipSlot, &str)]) -> Inventory {
        let mut inventory = Inventory::with_empty();
        for (slot, specifier) in slots {
            inventory.replace_loadout_item(
                *slot,
                Some(comp::Item::new_from_asset_expect(specifier)),
                Time(0.0),
            );
        }
        inventory
    }

    /// The params half of the wire format is reproduced by hand on this side
    /// rather than shared as a type, so it is pinned on both sides. This
    /// assertion is the twin of `portrait_gen`'s
    /// `default_params_serialize_to_the_pinned_shape`; if they ever disagree,
    /// exactly one of them fails and says so.
    #[test]
    fn default_params_serialize_to_the_pinned_shape() {
        assert_eq!(
            serde_json::to_string(&PortraitParams::default()).unwrap(),
            r#"{"size":256,"framing":"full_body_front","version":"p1"}"#
        );
    }

    #[test]
    fn a_request_carries_the_body_the_inventory_and_the_params() {
        let body = body();
        let inventory = wearing(&[(EquipSlot::Armor(ArmorSlot::Chest), CHEST)]);

        let json: serde_json::Value = serde_json::from_slice(
            &serde_json::to_vec(&PortraitRequest {
                body: &body,
                inventory: &inventory,
                params: PortraitParams::default(),
            })
            .unwrap(),
        )
        .unwrap();

        assert_eq!(json["params"]["version"], PORTRAIT_PARAMS_VERSION);
        assert!(json.get("body").is_some());
        assert!(json.get("inventory").is_some());
    }

    #[test]
    fn the_key_is_stable_for_an_unchanged_character() {
        let body = body();
        let inventory = wearing(&[(EquipSlot::Armor(ArmorSlot::Chest), CHEST)]);

        assert_eq!(
            appearance_key(&body, &inventory),
            appearance_key(&body, &inventory)
        );
    }

    /// Equipping and re-equipping shuffles the inventory's internal storage
    /// around without changing what the character wears. The key must not
    /// notice.
    #[test]
    fn the_key_ignores_how_the_inventory_got_that_way() {
        let body = body();

        let straight = wearing(&[
            (EquipSlot::Armor(ArmorSlot::Chest), CHEST),
            (EquipSlot::Armor(ArmorSlot::Legs), CHEST),
        ]);

        let mut roundabout = Inventory::with_empty();
        // Same end state, reached in the other order and through an
        // intermediate item that is then replaced.
        roundabout.replace_loadout_item(
            EquipSlot::Armor(ArmorSlot::Legs),
            Some(comp::Item::new_from_asset_expect(CHEST)),
            Time(0.0),
        );
        roundabout.replace_loadout_item(
            EquipSlot::Armor(ArmorSlot::Chest),
            Some(comp::Item::new_from_asset_expect(OTHER_CHEST)),
            Time(0.0),
        );
        roundabout.replace_loadout_item(
            EquipSlot::Armor(ArmorSlot::Chest),
            Some(comp::Item::new_from_asset_expect(CHEST)),
            Time(0.0),
        );

        assert_eq!(
            appearance_key(&body, &straight),
            appearance_key(&body, &roundabout)
        );
    }

    #[test]
    fn changing_any_visible_slot_changes_the_key() {
        let body = body();
        let bare = appearance_key(&body, &Inventory::with_empty());

        for (name, slot) in KEYED_SLOTS {
            let dressed = appearance_key(&body, &wearing(&[(*slot, CHEST)]));
            assert_ne!(
                dressed, bare,
                "equipping the {name} slot must change the appearance key"
            );
        }
    }

    #[test]
    fn every_keyed_slot_has_its_own_position_in_the_key() {
        let body = body();
        let mut seen = std::collections::HashSet::new();
        for (name, slot) in KEYED_SLOTS {
            assert!(
                seen.insert(appearance_key(&body, &wearing(&[(*slot, CHEST)]))),
                "the {name} slot produces a key another slot already produces"
            );
        }
    }

    #[test]
    fn changing_the_body_changes_the_key() {
        let inventory = Inventory::with_empty();
        let a = humanoid::Body::random();
        let mut b = a;
        b.hair_color = a.hair_color.wrapping_add(1);

        assert_ne!(
            appearance_key(&Body::Humanoid(a), &inventory),
            appearance_key(&Body::Humanoid(b), &inventory)
        );
    }

    #[test]
    fn the_key_is_prefixed_with_the_params_version() {
        let key = appearance_key(&body(), &Inventory::with_empty());
        assert!(
            key.starts_with(&format!("{PORTRAIT_PARAMS_VERSION}|")),
            "a version bump must be able to invalidate every key: {key}"
        );
    }

    #[test]
    fn the_etag_is_the_hex_sha256_of_the_key() {
        let key = appearance_key(&body(), &Inventory::with_empty());
        let etag = etag(&key);

        assert_eq!(etag.len(), 64);
        assert!(etag.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(etag, hex::encode(Sha256::digest(key.as_bytes())));
        assert_ne!(
            etag,
            super::etag(&format!("p2|{key}")),
            "a different key must not produce the same tag"
        );
    }

    #[test]
    fn if_none_match_is_matched_through_its_decorations() {
        let etag = etag("p1|whatever");

        for header in [
            etag.clone(),
            format!("\"{etag}\""),
            format!("W/\"{etag}\""),
            format!("\"something-else\", \"{etag}\""),
        ] {
            assert!(etag_matches(&header, &etag), "{header} should match");
        }

        for header in ["", "\"\"", "*", "\"not-this-one\""] {
            assert!(!etag_matches(header, &etag), "{header} should not match");
        }
    }
}

/// The service's own behaviour, driven against a stand-in renderer.
///
/// A fake rather than the real `portrait_gen`: these tests are about queueing,
/// caching, timeouts and exit codes, none of which need a real image, and
/// building the voxygen binary is not something a `-p xindeler-server` test run
/// can do. Unix-only because the stand-in is a shell script.
#[cfg(all(test, unix))]
mod service_tests {
    use super::*;
    use crate::persistence::{PersistedComponents, SqlLogMode};
    use common::{
        comp::{self, ActiveAbilities, CharacterClass},
        resources::Time,
    };
    use common_i18n::Content;
    use std::os::unix::fs::PermissionsExt;

    const OWNER: &str = "11111111-1111-1111-1111-111111111111";
    const STRANGER: &str = "22222222-2222-2222-2222-222222222222";
    const CHEST: &str = "common.items.armor.cloth_blue.chest";
    const IMAGE: &[u8] = b"FAKE-PORTRAIT-BYTES";

    /// Comfortably more than a stand-in renderer needs, and deliberately not
    /// tight: the renderer is spawned at `nice -n 19`, so on a machine already
    /// running the rest of this suite in parallel it can be starved for a
    /// surprisingly long time. Only the test that is *about* the timeout
    /// shortens this.
    const TEST_RENDER_TIMEOUT: Duration = Duration::from_secs(10);

    /// Short enough that the test does not sit through the real one, long
    /// enough not to fire on a merely slow render.
    const TEST_TIMEOUT_UNDER_TEST: Duration = Duration::from_secs(2);

    /// A temporary server database with a stand-in renderer beside it.
    struct Harness {
        dir: tempfile::TempDir,
        settings: Arc<RwLock<DatabaseSettings>>,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let settings = DatabaseSettings {
                db_dir: dir.path().to_path_buf(),
                sql_log_mode: SqlLogMode::Disabled,
            };
            crate::persistence::run_migrations(&settings);
            Self {
                dir,
                settings: Arc::new(RwLock::new(settings)),
            }
        }

        fn db_settings(&self) -> DatabaseSettings {
            self.settings.read().expect("not poisoned").clone()
        }

        /// Writes a stand-in `portrait_gen` whose body is `script`, and which
        /// records one line per invocation in a counter file. Drains stdin
        /// first, as the real renderer does.
        fn renderer(&self, name: &str, script: &str) -> PathBuf {
            self.raw_renderer(name, &format!("cat >/dev/null\n{script}"))
        }

        /// [`renderer`](Self::renderer) without the stdin drain, for the cases
        /// that are about a renderer misbehaving.
        fn raw_renderer(&self, name: &str, script: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            let counter = self.dir.path().join(format!("{name}.count"));
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho x >> '{}'\n{script}\n", counter.display()),
            )
            .expect("write the stand-in renderer");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make it executable");
            path
        }

        fn renders(&self, name: &str) -> usize {
            std::fs::read_to_string(self.dir.path().join(format!("{name}.count")))
                .map(|contents| contents.lines().count())
                .unwrap_or(0)
        }

        fn character(&self, uuid: &str, alias: &str, chest: Option<&str>) -> CharacterId {
            let mut inventory = Inventory::with_empty();
            if let Some(specifier) = chest {
                inventory.replace_loadout_item(
                    EquipSlot::Armor(ArmorSlot::Chest),
                    Some(comp::Item::new_from_asset_expect(specifier)),
                    Time(0.0),
                );
            }

            let body = Body::Humanoid(comp::humanoid::Body::random());
            let components = PersistedComponents {
                body,
                hardcore: None,
                character_class: CharacterClass::single(comp::ClassKind::Mage),
                stats: comp::Stats::new(Content::Plain(alias.to_owned()), body),
                skill_set: comp::SkillSet::default(),
                inventory,
                waypoint: None,
                pets: Vec::new(),
                active_abilities: ActiveAbilities::default(),
                map_marker: None,
                ethos: comp::Ethos::default(),
                background: comp::Background::default(),
                pact: comp::Pact::default(),
                trigger_slots: comp::TriggerSlots::default(),
                spell_mastery: comp::SpellMastery::default(),
            };

            crate::persistence::create_character_for_test(
                uuid,
                alias,
                components,
                &self.db_settings(),
            )
            .expect("character creation")
        }

        fn service(&self, renderer: PathBuf) -> PortraitService {
            self.service_with_timeout(renderer, TEST_RENDER_TIMEOUT)
        }

        fn service_with_timeout(
            &self,
            renderer: PathBuf,
            render_timeout: Duration,
        ) -> PortraitService {
            PortraitService {
                database_settings: Arc::clone(&self.settings),
                portrait_gen_path: renderer,
                render_timeout,
            }
        }
    }

    /// Blocks on one request through the handle, the way the HTTP layer will.
    fn ask(
        handle: &PortraitServiceHandle,
        uuid: &str,
        character_id: CharacterId,
        if_none_match: Option<&str>,
    ) -> PortraitOutcome {
        let (respond, answer) = tokio::sync::oneshot::channel();
        handle.request(PortraitRequestMsg {
            uuid: uuid.to_owned(),
            character_id,
            if_none_match: if_none_match.map(str::to_owned),
            respond,
        });
        answer.blocking_recv().expect("the worker answers")
    }

    /// The rest of these tests drive `answer` directly, which keeps them free
    /// of thread timing. This one goes the whole way round -- handle, queue,
    /// worker thread, response channel -- so that the wiring the frontend
    /// actually uses is exercised too.
    #[test]
    fn a_request_through_the_handle_comes_back_from_the_worker() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", Some(CHEST));
        let renderer = harness.renderer("handle", "printf 'FAKE-PORTRAIT-BYTES'");
        let handle = PortraitService::spawn_with(
            Arc::clone(&harness.settings),
            renderer,
            TEST_RENDER_TIMEOUT,
            QUEUE_DEPTH,
        );

        let outcome = ask(&handle, OWNER, id, None);
        assert!(
            matches!(&outcome, PortraitOutcome::Fresh { bytes, .. } if bytes == IMAGE),
            "{outcome:?}"
        );
        assert_eq!(
            ask(&handle, STRANGER, id, None),
            PortraitOutcome::NotFound,
            "ownership is enforced on the worker's side of the queue too"
        );
    }

    #[test]
    fn a_first_request_renders_and_a_second_one_does_not() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", Some(CHEST));
        let renderer = harness.renderer("ok", &format!("printf '{}'", "FAKE-PORTRAIT-BYTES"));
        let service = harness.service(renderer);

        let first = service.answer(OWNER, id, None);
        assert!(
            matches!(&first, PortraitOutcome::Fresh { bytes, format, .. }
                if bytes == IMAGE && format == PORTRAIT_FORMAT),
            "{first:?}"
        );
        assert_eq!(harness.renders("ok"), 1);

        let second = service.answer(OWNER, id, None);
        assert_eq!(first, second, "the cached image must come back unchanged");
        assert_eq!(
            harness.renders("ok"),
            1,
            "an unchanged character must be served from the cache"
        );
    }

    #[test]
    fn a_changed_loadout_renders_once_more_and_is_then_cached_again() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        let renderer = harness.renderer("stale", "printf 'FAKE-PORTRAIT-BYTES'");
        let service = harness.service(renderer);

        service.answer(OWNER, id, None);
        assert_eq!(harness.renders("stale"), 1);

        // Rewrite the cached row's appearance key, which is exactly what
        // equipping something in-game would eventually amount to from this
        // side: the persisted appearance no longer matches what was rendered.
        crate::persistence::upsert_portrait(
            id,
            "p1|a-completely-different-appearance",
            PORTRAIT_FORMAT,
            b"an-older-image",
            Utc::now(),
            &harness.db_settings(),
        )
        .expect("upsert");

        let outcome = service.answer(OWNER, id, None);
        assert!(
            matches!(&outcome, PortraitOutcome::Fresh { bytes, .. } if bytes == IMAGE),
            "a stale row must be replaced, not served: {outcome:?}"
        );
        assert_eq!(harness.renders("stale"), 2);

        service.answer(OWNER, id, None);
        assert_eq!(
            harness.renders("stale"),
            2,
            "the re-render must have been cached"
        );
    }

    #[test]
    fn a_matching_if_none_match_is_answered_without_rendering() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", Some(CHEST));
        let renderer = harness.renderer("cond", "printf 'FAKE-PORTRAIT-BYTES'");
        let service = harness.service(renderer);

        let PortraitOutcome::Fresh { etag, .. } = service.answer(OWNER, id, None) else {
            panic!("the first request renders");
        };
        assert_eq!(harness.renders("cond"), 1);

        let outcome = service.answer(OWNER, id, Some(&format!("\"{etag}\"")));
        assert_eq!(outcome, PortraitOutcome::NotModified { etag });
        assert_eq!(harness.renders("cond"), 1);

        let outcome = service.answer(OWNER, id, Some("\"a-stale-etag\""));
        assert!(
            matches!(outcome, PortraitOutcome::Fresh { .. }),
            "a non-matching If-None-Match must still be served the image"
        );
    }

    #[test]
    fn a_character_that_is_not_the_callers_is_not_found() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        let renderer = harness.renderer("owned", "printf 'FAKE-PORTRAIT-BYTES'");
        let service = harness.service(renderer);

        assert_eq!(
            service.answer(STRANGER, id, None),
            PortraitOutcome::NotFound
        );
        assert_eq!(
            service.answer(OWNER, CharacterId(987_654), None),
            PortraitOutcome::NotFound
        );
        assert_eq!(
            harness.renders("owned"),
            0,
            "nothing that fails its ownership check may reach the renderer"
        );
    }

    #[test]
    fn a_renderer_that_hangs_is_killed_and_nothing_is_cached() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        let renderer = harness.renderer("hang", "sleep 60");
        let service = harness.service_with_timeout(renderer, TEST_TIMEOUT_UNDER_TEST);

        let started = Instant::now();
        assert_eq!(service.answer(OWNER, id, None), PortraitOutcome::Failed);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the render must have been killed at its timeout, not waited out"
        );

        assert_eq!(
            crate::persistence::get_portrait(id, &harness.db_settings()).expect("get"),
            None,
            "a failed render must not be cached"
        );
    }

    #[test]
    fn a_renderer_that_refuses_the_request_fails_without_caching() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        // Exit 2 is the renderer's "bad request".
        let renderer = harness.renderer("refuse", "echo 'bad request' >&2; exit 2");
        let service = harness.service(renderer);

        assert_eq!(service.answer(OWNER, id, None), PortraitOutcome::Failed);
        assert_eq!(
            crate::persistence::get_portrait(id, &harness.db_settings()).expect("get"),
            None
        );
    }

    #[test]
    fn a_renderer_killed_by_a_signal_fails_without_caching() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        // No exit code at all -- the case a `panic = "abort"` build produces,
        // and the one the exit-code contract says must never be retried.
        let renderer = harness.renderer("signal", "kill -ABRT $$");
        let service = harness.service(renderer);

        assert_eq!(service.answer(OWNER, id, None), PortraitOutcome::Failed);
        assert_eq!(
            crate::persistence::get_portrait(id, &harness.db_settings()).expect("get"),
            None
        );
    }

    #[test]
    fn a_renderer_that_draws_nothing_is_a_failure_not_an_empty_portrait() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        let renderer = harness.renderer("empty", "true");
        let service = harness.service(renderer);

        assert_eq!(service.answer(OWNER, id, None), PortraitOutcome::Failed);
        assert_eq!(
            crate::persistence::get_portrait(id, &harness.db_settings()).expect("get"),
            None
        );
    }

    #[test]
    fn a_renderer_that_draws_too_much_is_refused_rather_than_buffered() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        // Comfortably past MAX_IMAGE_BYTES, and never reading its request, so
        // both the output cap and the pipe handling are under test at once.
        let renderer = harness.raw_renderer("huge", "yes x | head -c 1200000");
        let service = harness.service(renderer);

        assert_eq!(service.answer(OWNER, id, None), PortraitOutcome::Failed);
        assert_eq!(
            crate::persistence::get_portrait(id, &harness.db_settings()).expect("get"),
            None
        );
    }

    /// The renderer is handed its request by a thread of its own, so a
    /// renderer that never reads stdin cannot wedge the worker -- with the
    /// write on the worker's own thread, a request bigger than a pipe buffer
    /// would block there forever, where the timeout cannot reach it.
    #[test]
    fn a_renderer_that_ignores_its_request_is_still_answered() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", Some(CHEST));
        let renderer = harness.raw_renderer("deaf", "printf 'FAKE-PORTRAIT-BYTES'");
        let service = harness.service(renderer);

        let outcome = service.answer(OWNER, id, None);
        assert!(
            matches!(&outcome, PortraitOutcome::Fresh { bytes, .. } if bytes == IMAGE),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_missing_renderer_fails_the_request_rather_than_the_server() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        let service = harness.service(harness.dir.path().join("not-installed"));

        assert_eq!(service.answer(OWNER, id, None), PortraitOutcome::Failed);
    }

    #[test]
    fn requests_past_the_queue_are_told_to_come_back_later() {
        let harness = Harness::new();
        let id = harness.character(OWNER, "Testificate", None);
        // Slow enough that the worker is still busy with the first request
        // while the rest arrive.
        let renderer = harness.renderer("slow", "sleep 0.4; printf 'FAKE-PORTRAIT-BYTES'");
        let handle = PortraitService::spawn_with(
            Arc::clone(&harness.settings),
            renderer,
            TEST_RENDER_TIMEOUT,
            1,
        );

        let mut answers = Vec::new();
        for _ in 0..4 {
            let (respond, answer) = tokio::sync::oneshot::channel();
            handle.request(PortraitRequestMsg {
                uuid: OWNER.to_owned(),
                character_id: id,
                if_none_match: None,
                respond,
            });
            answers.push(answer);
        }

        let outcomes: Vec<_> = answers
            .into_iter()
            .map(|answer| answer.blocking_recv().expect("every request is answered"))
            .collect();

        assert!(
            outcomes.contains(&PortraitOutcome::Busy),
            "a full queue must answer Busy rather than enqueue without bound: {outcomes:?}"
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, PortraitOutcome::Fresh { .. })),
            "the requests that did fit must still be served: {outcomes:?}"
        );
    }
}
