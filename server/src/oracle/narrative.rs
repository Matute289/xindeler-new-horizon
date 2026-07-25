//! DM-flavour text delivery: [`common_oracle::Narrative::on_enter_message`]
//! greets the one client whose character enters an event, fired once.
//!
//! Reuses `ChatType::Meta` rather than adding a new `ServerGeneral::
//! Notification` variant: `Notification` currently has exactly one variant
//! (`WaypointSaved`), and adding a second is a wire-format change, whereas
//! `ChatType::Meta` already exists, is invisible to normal chat filtering,
//! and costs no protocol change to reuse.

use common::comp::{ChatType, Content};
use common_net::msg::ServerGeneral;
use specs::Entity as EcsEntity;

use crate::Server;

/// Sends `message` to `entity` once, as a `ChatType::Meta` server message. A
/// no-op if `entity` has no `Client` (already `notify_client`'s own
/// contract).
pub fn send_on_enter_message(server: &Server, entity: EcsEntity, message: &str) {
    server.notify_client(
        entity,
        ServerGeneral::server_msg(ChatType::Meta, Content::Plain(message.to_owned())),
    );
}
