use crate::oracle::OracleWatcher;
use common_ecs::{Job, Origin, Phase, System};
use specs::WriteExpect;

/// Drains [`OracleWatcher`]'s pending filesystem changes once per tick,
/// ingesting or retiring its in-memory event table. All the actual
/// parsing/sanitizing happens inside `OracleWatcher::poll` — this system is
/// just the tick-driven trigger, same shape as `chunk_generator`'s own
/// channel-drain pattern.
#[derive(Default)]
pub struct Sys;
impl<'a> System<'a> for Sys {
    type SystemData = WriteExpect<'a, OracleWatcher>;

    const NAME: &'static str = "oracle";
    const ORIGIN: Origin = Origin::Server;
    const PHASE: Phase = Phase::Create;

    fn run(_job: &mut Job<Self>, mut watcher: Self::SystemData) { watcher.poll(); }
}
