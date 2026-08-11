//! PROJECT ORACLE server-side seams: a filesystem-driven event schema an
//! operator (or, later, an offline tool) drops into a watched directory to
//! spin up a hand-authored encounter. See `xindeler_common_oracle` for the
//! data schema and anti-chaos validation; this module is the server-side
//! runtime that watches for files and turns them into live state.

pub mod chronicle;
pub mod factory;
pub mod narrative;
pub mod policy;
pub mod spawned;
pub mod trigger;
pub mod watcher;

pub use chronicle::ChronicleLog;
pub use policy::{OracleEventsEnabled, OracleTriggerLedger};
pub use spawned::OracleSpawned;
pub use watcher::{OracleAsset, OracleEvents, OracleWatcher};
