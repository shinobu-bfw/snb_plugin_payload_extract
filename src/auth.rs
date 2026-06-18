use crate::config;

use super::CommandRequest;

pub(super) fn is_admin(request: &CommandRequest, cfg: &config::Config) -> bool {
    if request.is_admin {
        return true;
    }

    let Some(user_id) = request
        .from
        .as_deref()
        .and_then(|from| from.parse::<i64>().ok())
    else {
        log::warn!("admin command rejected: no sender info");
        return false;
    };

    if cfg.admin_users.contains(&user_id) {
        return true;
    }

    log::warn!("admin command rejected: user {user_id} is not an admin");
    false
}
