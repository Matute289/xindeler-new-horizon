//! Free-standing Warlock-pact mutation, kept separate from the `/pact` chat
//! command (`cmd.rs`) so a future quest/event trigger can bind or sever a
//! pact directly, without depending on the admin-command dispatcher for
//! domain logic. Mirrors `oracle::narrative::send_on_enter_message`'s shape:
//! a plain function taking `&mut Server`, not a `StateExt` method, since it
//! needs to notify the client (`Server::notify_client`), not just mutate the
//! ECS `State`.

use common::comp::{
    ChatType, Content,
    pact::{Pact, PactStanding},
};
use common_net::msg::ServerGeneral;
use specs::{Entity as EcsEntity, WorldExt};

use crate::Server;

/// Writes `pact` onto `target`. If this write is the moment the standing
/// becomes `Severed` (was not already), also sends the target a
/// `ChatType::Meta` break-moment notice -- the same channel narrative-event
/// greetings use (`oracle::narrative::send_on_enter_message`), but
/// localized rather than freeform, since this is a fixed system message
/// rather than authored event content.
pub fn set_pact(server: &mut Server, target: EcsEntity, pact: Pact) {
    let was_severed = server
        .state
        .ecs()
        .read_storage::<Pact>()
        .get(target)
        .is_some_and(|p| p.standing == PactStanding::Severed);
    let now_severed = pact.standing == PactStanding::Severed;

    let _ = server
        .state
        .ecs_mut()
        .write_storage::<Pact>()
        .insert(target, pact);

    if now_severed && !was_severed {
        server.notify_client(
            target,
            ServerGeneral::server_msg(
                ChatType::Meta,
                Content::localized("hud-pact-severed-notice"),
            ),
        );
    }
}
