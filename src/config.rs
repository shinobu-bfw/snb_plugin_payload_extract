use serde::{Deserialize, Serialize};

/// Plugin configuration, loaded from `configs/PayloadExtractBot/config.toml`.
///
/// Only the fields the plugin actually uses are kept: the partition allow-list
/// for `/dump` and the admin user IDs for `/update` and `/status`. The Telegram
/// token and API URL live in the adapter's config
/// (`configs/TGAdapter/config.toml`), not here.
#[derive(Deserialize, Serialize)]
pub struct Config {
    /// Partitions allowed for `/dump`. Leave empty to allow all partitions.
    #[serde(rename = "SUPPORTED_PARTITIONS")]
    pub supported_partitions: Vec<String>,
    /// Telegram user IDs allowed to run admin-only commands (`/update`, `/status`).
    ///
    /// Adapter-provided `message.is_admin = true` is also accepted (see
    /// [`crate::auth::is_admin`]).
    #[serde(rename = "ADMIN_USERS", default)]
    pub admin_users: Vec<i64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            supported_partitions: vec![
                "boot".to_string(),
                "dtbo".to_string(),
                "init_boot".to_string(),
                "modem".to_string(),
                "modemfirmware".to_string(),
                "recovery".to_string(),
                "system_dlkm".to_string(),
                "vbmeta".to_string(),
                "vbmeta_system".to_string(),
                "vbmeta_vendor".to_string(),
                "vendor_boot".to_string(),
                "vendor_dlkm".to_string(),
            ],
            admin_users: vec![],
        }
    }
}
