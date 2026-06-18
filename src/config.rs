use serde::{Deserialize, Serialize};

/// Plugin configuration, loaded from `configs/PayloadExtract/config.toml`.
///
/// The Telegram token and API URL live in the adapter's config
/// (`configs/TGAdapter/config.toml`), not here.
#[derive(Deserialize, Serialize)]
pub struct Config {
    /// Partitions allowed for `/dump`. Leave empty to allow all partitions.
    ///
    /// Entries may use glob wildcards: `*` matches any run of characters and `?`
    /// matches a single one (e.g. `xbl*` allows `xbl_a`, `xbl_config_b`, …).
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
                "abl*".to_string(),
                "boot".to_string(),
                "dtbo".to_string(),
                "init_boot".to_string(),
                "modem".to_string(),
                "modemfirmware".to_string(),
                "recovery".to_string(),
                "system_dlkm".to_string(),
                "vbmeta*".to_string(),
                "vendor_boot".to_string(),
                "vendor_dlkm".to_string(),
                "xbl*".to_string(),
            ],
            admin_users: vec![],
        }
    }
}
