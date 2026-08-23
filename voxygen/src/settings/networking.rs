use hashbrown::HashSet;
use serde::{Deserialize, Serialize};

/// Auth host pre-trusted ahead of the server-side cutover to it, so it's
/// present in both `NetworkingSettings::default()` and the `Settings::load`
/// migration that inserts it into already-persisted `settings.ron` files.
pub const VINZCLORTHO_AUTH_HOST: &str = "https://vinzclortho.xindeler.com";

/// `NetworkingSettings` stores server and networking settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkingSettings {
    pub username: String,
    pub servers: Vec<String>,
    pub default_server: String,
    pub trusted_auth_servers: HashSet<String>,
    pub use_srv: bool,
    pub use_quic: bool,
    pub validate_tls: bool,
    pub player_physics_behavior: bool,
    pub lossy_terrain_compression: bool,
    pub enable_discord_integration: bool,
    pub bug_report_url: Option<String>,
}

impl Default for NetworkingSettings {
    fn default() -> Self {
        Self {
            username: "".to_string(),
            servers: vec!["server.xindeler.com".to_string()],
            default_server: "server.xindeler.com".to_string(),
            // Trusted ahead of the server-side cutover to this host so fresh
            // installs don't hit the untrusted-auth-server prompt the moment
            // a server starts advertising it as `auth_provider`.
            trusted_auth_servers: ["https://auth.xindeler.com", VINZCLORTHO_AUTH_HOST]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            use_srv: true,
            use_quic: false,
            validate_tls: true,
            player_physics_behavior: false,
            lossy_terrain_compression: false,
            enable_discord_integration: true,
            bug_report_url: None,
        }
    }
}
