use std::path::{Path, PathBuf};
use std::time::Duration;

use payload_extract_bot::{patch_boot, payload};
use snb_core::context;
use snb_core::event::{ContentItem, Event, EventType, FileSource, Message, TextFormat};

use super::{CommandRequest, HINT_DELETE_DELAY, PLUGIN_NAME};

pub(super) fn dump_caption_markdown(files: &[payload::PartitionInfo]) -> String {
    let mut out = String::new();
    for file in files {
        out.push_str(&format!(
            "> `{}`\\(`{}`\\): `{}`\n>\n",
            escape_markdown_v2_code(&file.name),
            escape_markdown_v2_code(&format_size(file.size)),
            escape_markdown_v2_code(file.hash.as_deref().unwrap_or("N/A").trim_matches('"'))
        ));
    }
    out
}

pub(super) fn patch_caption_markdown(patched: &patch_boot::PatchedFile) -> String {
    format!(
        ">Patch Method: `{}`\n>Patch Version: `{}`\n>KMI: `{}`\n>Kernel Version: `{}`",
        escape_markdown_v2_code(patched.patch_method()),
        escape_markdown_v2_code(patched.patch_version()),
        escape_markdown_v2_code(patched.kmi()),
        escape_markdown_v2_code(patched.kernel_version())
    )
}

pub(super) fn emit_text(request: &CommandRequest, text: impl Into<String>) {
    let text = text.into();
    for chunk in split_message(&text) {
        emit_content(request, None, None, vec![text_item(chunk, None)]);
    }
}

pub(super) fn emit_formatted_text(
    request: &CommandRequest,
    text: impl Into<String>,
    format: TextFormat,
) {
    let text = text.into();
    for chunk in split_message(&text) {
        emit_content(request, None, None, vec![text_item(chunk, Some(format))]);
    }
}

pub(super) fn emit_html_blockquote(request: &CommandRequest, text: impl Into<String>) {
    let text = text.into();
    for chunk in split_message(&text) {
        emit_content(
            request,
            None,
            None,
            vec![text_item(html_blockquote(&chunk), Some(TextFormat::Html))],
        );
    }
}

pub(super) fn emit_temporary_text(request: &CommandRequest, text: impl Into<String>) {
    emit_content(
        request,
        None,
        Some(HINT_DELETE_DELAY),
        vec![text_item(text.into(), None)],
    );
}

pub(super) fn emit_status_text(request: &CommandRequest, text: impl Into<String>) {
    emit_content(
        request,
        None,
        Some(HINT_DELETE_DELAY),
        vec![text_item(text.into(), None)],
    );
}

pub(super) fn emit_files_with_caption(
    request: &CommandRequest,
    id: Option<String>,
    caption: impl Into<String>,
    files: &[payload::PartitionInfo],
) {
    let mut content = vec![text_item(caption.into(), Some(TextFormat::MarkdownV2))];
    content.extend(files.iter().map(|file| ContentItem::File {
        source: FileSource::Path(file.path.to_string_lossy().into_owned()),
        file_name: file_name(&file.path),
        file_id: None,
    }));
    emit_content(request, id, None, content);
}

pub(super) fn emit_file_with_caption(
    request: &CommandRequest,
    id: Option<String>,
    caption: impl Into<String>,
    path: PathBuf,
    file_name: Option<String>,
) {
    emit_content(
        request,
        id,
        None,
        vec![
            text_item(caption.into(), Some(TextFormat::MarkdownV2)),
            ContentItem::File {
                source: FileSource::Path(path.to_string_lossy().into_owned()),
                file_name,
                file_id: None,
            },
        ],
    );
}

pub(super) fn emit_content(
    request: &CommandRequest,
    id: Option<String>,
    delete_after: Option<Duration>,
    content: Vec<ContentItem>,
) {
    let event = Event {
        event_type: EventType::Message,
        source: PLUGIN_NAME.to_string(),
        data: String::new(),
        command: None,
        message: Some(Message {
            id,
            reply_to: request.reply_to.clone(),
            content,
            from: None,
            to: request.to.clone(),
            at: Vec::new(),
            chat_type: None,
            is_admin: false,
            delete_after,
        }),
        sender: None,
        receiver: request.receiver.clone(),
    };
    context::bot().emit_event(event);
}

pub(super) fn text_item(text: impl Into<String>, format: Option<TextFormat>) -> ContentItem {
    ContentItem::Text {
        text: text.into(),
        format,
    }
}

fn html_blockquote(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!("<blockquote expandable>{escaped}</blockquote>")
}

fn escape_markdown_v2_code(text: &str) -> String {
    text.replace('\\', "\\\\").replace('`', "\\`")
}

fn split_message(text: &str) -> Vec<String> {
    const MAX_LEN: usize = 3500;
    if text.is_empty() {
        return vec![String::new()];
    }
    if text.len() <= MAX_LEN {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line_len = line.len() + usize::from(!current.is_empty());
        if !current.is_empty() && current.len() + line_len > MAX_LEN {
            chunks.push(std::mem::take(&mut current));
        }
        if line.len() > MAX_LEN {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut buf = String::new();
            for ch in line.chars() {
                if buf.len() + ch.len_utf8() > MAX_LEN {
                    chunks.push(std::mem::take(&mut buf));
                }
                buf.push(ch);
            }
            if !buf.is_empty() {
                chunks.push(buf);
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub(super) fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }
    format!("{value:.1} {}", UNITS[unit_index])
}
