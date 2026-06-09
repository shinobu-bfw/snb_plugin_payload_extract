use super::*;

fn request(from: Option<&str>, is_admin: bool) -> CommandRequest {
    CommandRequest {
        args: String::new(),
        to: None,
        reply_to: None,
        receiver: None,
        from: from.map(str::to_string),
        is_admin,
    }
}

#[test]
fn wrapper_admin_flag_allows_admin_commands() {
    let cfg = config::Config::default();
    assert!(is_admin(&request(None, true), &cfg));
}

#[test]
fn config_admin_list_still_allows_admin_commands() {
    let mut cfg = config::Config::default();
    cfg.admin_users = vec![42];
    assert!(is_admin(&request(Some("42"), false), &cfg));
}

#[test]
fn non_admin_is_denied() {
    let cfg = config::Config::default();
    assert!(!is_admin(&request(Some("42"), false), &cfg));
}
