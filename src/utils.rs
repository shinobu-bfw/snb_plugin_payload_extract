use teloxide::prelude::{Message, ResponseResult};
use log::info;

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36";

pub fn to_tg_md(s: String) -> String {
    s.replace("-", "\\-")
        .replace(".", "\\.")
        .replace("(", "\\(")
        .replace(")", "\\)")
        .replace("+", "\\+")
        .replace("#", "\\#")
}

pub async fn log_message(msg: Message) -> ResponseResult<()> {
    let sender = msg
        .from
        .as_ref()
        .map(|user| user.full_name())
        .unwrap_or_else(|| "Unknown".to_string());
    let sender_id = msg
        .from
        .as_ref()
        .map(|user| user.id.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let group_name = msg.chat.title().unwrap_or("Private");
    let group_id = msg.chat.id;

    if let Some(text) = msg.text() {
        info!("{sender}({sender_id}) [{group_name}({group_id})] : {text}");
    } else {
        info!("{sender}({sender_id}) [{group_name}({group_id})] : <non-text message>");
    }
    Ok(())
}
