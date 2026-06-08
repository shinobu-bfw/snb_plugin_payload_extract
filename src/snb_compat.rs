//! Shinobu compatibility layer for payload_extract_bot-rs.

use payload_extract_bot::{config, patch_boot, payload, tool, utils};

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use snb_core::command::CommandContext;
use snb_core::context::{self, BotContext};
use snb_core::event::{ContentItem, Event, EventType, FileSource, Message, TextFormat};
use snb_core::plugin::{PluginType, SnbPlugin, Version};
use snb_macros::{command, plugin};

const PLUGIN_NAME: &str = "PayloadExtractBot";
const CLEANUP_DELAY: Duration = Duration::from_secs(30 * 60);
const HINT_DELETE_DELAY: Duration = Duration::from_secs(10);

const HELP_MESSAGE: &str = r#"<b><a href="https://github.com/kmiit/payload_dump_bot-rs">Payload dumper bot written in rust</a>.</b>

<blockquote expandable><b>Usage:</b>
<code>/dump [url] [partition1&lt;,partition2,partition3...&gt;]</code>
Dump partition(s) from url

<code>/list [url]</code>
List partition info of url

<code>/meta [url]</code>
Show OTA metadata from the OTA zip

<code>/patch [url] [partition] [kmi]</code>
Patch a boot partition with KernelSU
partition: boot(b), init_boot(ib), vendor_boot(vb)
kmi: optional

<code>/update</code>
Update ksud and magiskboot tools to latest version

<code>/status</code>
Show current bot status

<code>/help</code>
Show this help msg.</blockquote>"#;

struct State {
    cfg: Arc<config::Config>,
    tm: Arc<tool::ToolManager>,
}

/// Tracks a live status message whose native id is only known once the adapter
/// echoes a [`EventType::MessageSent`] back. Edits/deletes requested before that
/// point are stashed and flushed when the id arrives.
struct StatusState {
    to: Option<String>,
    receiver: Option<String>,
    platform_id: Option<String>,
    pending_text: Option<String>,
    pending_delete: bool,
}

#[derive(Clone)]
struct CommandRequest {
    args: String,
    to: Option<String>,
    reply_to: Option<String>,
    receiver: Option<String>,
    from: Option<String>,
}

#[derive(Clone, Copy)]
enum CommandKind {
    Dump,
    List,
    Meta,
    Patch,
    Update,
    Status,
    Help,
}

static STATE: RwLock<Option<Arc<State>>> = RwLock::new(None);
static NEXT_CLEANUP_ID: AtomicU64 = AtomicU64::new(1);
static PENDING_CLEANUPS: LazyLock<Mutex<HashMap<String, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_STATUS_ID: AtomicU64 = AtomicU64::new(1);
static PENDING_STATUS: LazyLock<Mutex<HashMap<String, StatusState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[plugin]
struct PayloadExtractBot;

impl SnbPlugin for PayloadExtractBot {
    fn new() -> Self {
        Self
    }

    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn version(&self) -> Version {
        Version {
            major: 0,
            minor: 1,
            patch: 3,
        }
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Plugin
    }

    fn on_load(&mut self, ctx: Arc<dyn BotContext>) {
        context::set_bot(ctx.clone());

        match init_state(ctx.as_ref(), self.name()) {
            Ok(state) => {
                *STATE.write().unwrap() = Some(Arc::new(state));
                context::register_all(self.name());
                log::info!("v{} loaded!", self.version());
            }
            Err(e) => {
                log::error!("failed to load {PLUGIN_NAME}: {e:#}");
            }
        }
    }

    fn on_unload(&mut self) {
        *STATE.write().unwrap() = None;
        log::info!("unloaded!");
    }

    fn on_event(&self, event: &Event) {
        if event.event_type != EventType::MessageSent {
            return;
        }

        let Some(message) = event.message.as_ref() else {
            return;
        };
        let Some(local_id) = message.reply_to.as_deref() else {
            return;
        };
        cleanup_registered_path(local_id);
        if let Some(platform_id) = message.id.as_deref() {
            resolve_status_message(local_id, platform_id);
        }
    }
}

fn init_state(ctx: &dyn BotContext, plugin_name: &str) -> anyhow::Result<State> {
    let cfg = Arc::new(load_plugin_config(ctx, plugin_name)?);
    let bin_root = ctx.data_dir(plugin_name).join("bin");
    let tm = Arc::new(tool::ToolManager::try_with_bin_root(bin_root)?);
    Ok(State { cfg, tm })
}

fn load_plugin_config(ctx: &dyn BotContext, plugin_name: &str) -> anyhow::Result<config::Config> {
    let config_path = Path::new("PayloadExtractBot/config.toml");
    match ctx.load_config(config_path) {
        Ok(content) => toml::from_str(&content).context("failed to parse PayloadExtractBot config"),
        Err(_) => {
            let default = config::Config::default();
            let content =
                toml::to_string_pretty(&default).context("failed to render default config")?;
            ctx.write_config(plugin_name, Path::new("config.toml"), &content)
                .context("failed to write default PayloadExtractBot config")?;
            log::warn!(
                "config not found, default config written to configs/{PLUGIN_NAME}/config.toml"
            );
            Ok(default)
        }
    }
}

fn state() -> anyhow::Result<Arc<State>> {
    STATE
        .read()
        .unwrap()
        .as_ref()
        .cloned()
        .context("PayloadExtractBot is not initialized")
}

#[command(name = "dump", aliases = ["dumper"])]
fn dump(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Dump)
}

#[command(name = "list")]
fn list(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::List)
}

#[command(name = "meta", aliases = ["metadata"])]
fn meta(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Meta)
}

#[command(name = "patch")]
fn patch(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Patch)
}

#[command(name = "update")]
fn update(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Update)
}

#[command(name = "status")]
fn status(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Status)
}

#[command(name = "help", aliases = ["start"])]
fn help(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Help)
}

fn run_command(ctx: &CommandContext, kind: CommandKind) -> anyhow::Result<()> {
    let request = CommandRequest::from_context(ctx);
    spawn_task(async move {
        if let Err(e) = execute_command(kind, request.clone()).await {
            log::error!("PayloadExtractBot command failed: {e:#}");
            emit_text(&request, format!("Command failed: {e:#}"));
        }
    });
    Ok(())
}

impl CommandRequest {
    fn from_context(ctx: &CommandContext) -> Self {
        let msg = ctx.event.message.as_ref();
        Self {
            args: ctx.args.to_string(),
            to: msg.and_then(|m| m.to.clone()),
            reply_to: msg.and_then(|m| m.id.clone()),
            receiver: ctx.event.sender.clone(),
            from: msg.and_then(|m| m.from.clone()),
        }
    }
}

fn spawn_task<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    std::thread::spawn(move || snb_core::adapter::run_async(future));
}

async fn execute_command(kind: CommandKind, request: CommandRequest) -> anyhow::Result<()> {
    match kind {
        CommandKind::Help => {
            emit_formatted_text(&request, HELP_MESSAGE, TextFormat::Html);
            Ok(())
        }
        CommandKind::Dump => dump_command(request).await,
        CommandKind::List => list_command(request).await,
        CommandKind::Meta => meta_command(request).await,
        CommandKind::Patch => patch_command(request).await,
        CommandKind::Update => update_command(request).await,
        CommandKind::Status => status_command(request).await,
    }
}

async fn dump_command(request: CommandRequest) -> anyhow::Result<()> {
    let state = state()?;
    let args = request.args.split_whitespace().collect::<Vec<_>>();
    if args.len() != 2 {
        emit_temporary_text(
            &request,
            "Invalid command! Usage: /dump <url> <partition1,partition2,...>",
        );
        return Ok(());
    }

    let url = args[0].to_string();
    let partition = args[1].to_string();
    let unsupported = unsupported_partitions(&partition, &state.cfg.supported_partitions);
    if !unsupported.is_empty() {
        emit_temporary_text(
            &request,
            format!("Partition {} is not supported!", unsupported.join(", ")),
        );
        return Ok(());
    }

    emit_status_text(&request, format!("Dumping {partition}..."));
    let (files, temp_dir) = match payload::dump_partition(url, partition.clone()).await {
        Ok(result) => result,
        Err(e) => {
            emit_text(&request, format!("Failed to dump partitions: {e:#}"));
            return Ok(());
        }
    };

    if files.is_empty() {
        cleanup_path_now(temp_dir);
        emit_text(&request, "No dumped file found.");
        return Ok(());
    }

    let cleanup_id = register_cleanup_path(temp_dir);
    emit_files_with_caption(
        &request,
        Some(cleanup_id),
        dump_caption_markdown(&files),
        &files,
    );
    Ok(())
}

async fn list_command(request: CommandRequest) -> anyhow::Result<()> {
    let Some(url) = request.args.split_whitespace().next().map(str::to_string) else {
        emit_temporary_text(&request, "Invalid command! Usage: /list <url>");
        return Ok(());
    };

    emit_status_text(&request, "Fetching payload partition list...");
    match payload::list_image(url).await {
        Ok(text) => emit_html_blockquote(&request, text),
        Err(e) => emit_text(
            &request,
            format!("Failed to fetch payload partition list: {e:#}"),
        ),
    }
    Ok(())
}

async fn meta_command(request: CommandRequest) -> anyhow::Result<()> {
    let Some(url) = request.args.split_whitespace().next().map(str::to_string) else {
        emit_temporary_text(&request, "Invalid command! Usage: /meta <url>");
        return Ok(());
    };

    emit_status_text(&request, "Fetching OTA metadata...");
    match payload::read_ota_metadata(url).await {
        Ok(text) => emit_html_blockquote(&request, text),
        Err(e) => emit_text(&request, format!("Failed to fetch OTA metadata: {e:#}")),
    }
    Ok(())
}

async fn patch_command(request: CommandRequest) -> anyhow::Result<()> {
    let state = state()?;
    let args = request.args.split_whitespace().collect::<Vec<_>>();
    if args.len() < 2 || args.len() > 3 {
        emit_temporary_text(
            &request,
            "Invalid command! Usage: /patch <url> <partition> [kmi]",
        );
        return Ok(());
    }

    let url = args[0].to_string();
    let partition = args[1].to_string();
    let kmi = args.get(2).map(|s| (*s).to_string());

    let status = StatusHandle::emit(
        &request,
        match &kmi {
            Some(kmi) => format!("Patching {partition} with KernelSU (KMI: {kmi})..."),
            None => format!("Patching {partition} with KernelSU..."),
        },
    );

    if let Err(e) = state.tm.init().await {
        status.finish();
        emit_text(&request, format!("Failed to initialize tools: {e:#}"));
        return Ok(());
    }
    let progress = {
        let status = status.clone();
        Arc::new(move |text: &str| status.update(text.to_string())) as patch_boot::ProgressFn
    };
    let patched =
        match patch_boot::patch_boot(url, partition.clone(), kmi, state.tm.clone(), progress).await {
            Ok(patched) => patched,
            Err(e) => {
                status.finish();
                emit_text(&request, format!("Failed to patch {partition}: {e:#}"));
                return Ok(());
            }
        };

    if !patched.path().exists() {
        status.finish();
        emit_text(
            &request,
            format!("Patched file {} not found!", patched.path().display()),
        );
        if let Some(dir) = patched.path().parent() {
            cleanup_path_now(dir.to_path_buf());
        }
        return Ok(());
    }

    let cleanup_id = patched
        .path()
        .parent()
        .map(Path::to_path_buf)
        .map(register_cleanup_path);
    emit_file_with_caption(
        &request,
        cleanup_id,
        patch_caption_markdown(&patched),
        patched.path().to_path_buf(),
        file_name(patched.path()),
    );
    status.finish();
    Ok(())
}

async fn update_command(request: CommandRequest) -> anyhow::Result<()> {
    let state = state()?;
    if !is_admin(&request, &state.cfg) {
        return Ok(());
    }

    emit_status_text(&request, "Updating tools...");
    match state.tm.update().await {
        Ok(()) => emit_temporary_text(&request, "Tools updated successfully!"),
        Err(e) => emit_text(&request, format!("Failed to update tools: {e:#}")),
    }
    Ok(())
}

async fn status_command(request: CommandRequest) -> anyhow::Result<()> {
    let state = state()?;
    if !is_admin(&request, &state.cfg) {
        return Ok(());
    }

    emit_text(&request, format!("{}", utils::get_sysinfo()));
    Ok(())
}

fn unsupported_partitions(partition: &str, supported: &[String]) -> Vec<String> {
    if supported.is_empty() {
        return Vec::new();
    }

    partition
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter(|name| !supported.iter().any(|supported| supported == *name))
        .map(ToOwned::to_owned)
        .collect()
}

fn is_admin(request: &CommandRequest, cfg: &config::Config) -> bool {
    let Some(user_id) = request
        .from
        .as_deref()
        .and_then(|from| from.parse::<i64>().ok())
    else {
        log::warn!("admin command rejected: no sender info");
        return false;
    };

    if !cfg.admin_users.is_empty() && cfg.admin_users.contains(&user_id) {
        return true;
    }

    log::warn!("admin command rejected: user {user_id} is not an admin");
    false
}

fn dump_caption_markdown(files: &[payload::PartitionInfo]) -> String {
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

fn patch_caption_markdown(patched: &patch_boot::PatchedFile) -> String {
    format!(
        ">Patch Method: `{}`\n>Patch Version: `{}`\n>KMI: `{}`\n>Kernel Version: `{}`",
        escape_markdown_v2_code(patched.patch_method()),
        escape_markdown_v2_code(patched.patch_version()),
        escape_markdown_v2_code(patched.kmi()),
        escape_markdown_v2_code(patched.kernel_version())
    )
}

fn emit_text(request: &CommandRequest, text: impl Into<String>) {
    let text = text.into();
    for chunk in split_message(&text) {
        emit_content(request, None, None, vec![text_item(chunk, None)]);
    }
}

fn emit_formatted_text(request: &CommandRequest, text: impl Into<String>, format: TextFormat) {
    let text = text.into();
    for chunk in split_message(&text) {
        emit_content(request, None, None, vec![text_item(chunk, Some(format))]);
    }
}

fn emit_html_blockquote(request: &CommandRequest, text: impl Into<String>) {
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

fn emit_temporary_text(request: &CommandRequest, text: impl Into<String>) {
    emit_content(
        request,
        None,
        Some(HINT_DELETE_DELAY),
        vec![text_item(text.into(), None)],
    );
}

fn emit_status_text(request: &CommandRequest, text: impl Into<String>) {
    emit_content(
        request,
        None,
        Some(HINT_DELETE_DELAY),
        vec![text_item(text.into(), None)],
    );
}

/// A status message that updates in place as work progresses and is deleted on
/// completion. The message is emitted with a local id; the native platform id
/// arrives later via a `MessageSent` event (see [`resolve_status_message`]), so
/// updates requested before then are stashed and flushed once it is known.
///
/// Cloning is cheap (just routing ids) — the shared state lives in
/// `PENDING_STATUS` keyed by `local_id`, so a clone handed to a progress
/// callback drives the same message.
#[derive(Clone)]
struct StatusHandle {
    local_id: String,
    to: Option<String>,
    receiver: Option<String>,
}

impl StatusHandle {
    /// Emit the initial status message (no auto-delete) and start tracking it.
    fn emit(request: &CommandRequest, text: impl Into<String>) -> Self {
        let local_id = next_status_id();
        PENDING_STATUS.lock().unwrap().insert(
            local_id.clone(),
            StatusState {
                to: request.to.clone(),
                receiver: request.receiver.clone(),
                platform_id: None,
                pending_text: None,
                pending_delete: false,
            },
        );
        emit_content(
            request,
            Some(local_id.clone()),
            None,
            vec![text_item(text.into(), None)],
        );
        Self {
            local_id,
            to: request.to.clone(),
            receiver: request.receiver.clone(),
        }
    }

    /// Replace the status text. Edits in place once the native id is known,
    /// otherwise stashes the latest text for [`resolve_status_message`].
    fn update(&self, text: impl Into<String>) {
        let text = text.into();
        let platform_id = {
            let mut map = PENDING_STATUS.lock().unwrap();
            let Some(state) = map.get_mut(&self.local_id) else {
                return;
            };
            match &state.platform_id {
                Some(id) => id.clone(),
                None => {
                    state.pending_text = Some(text);
                    return;
                }
            }
        };
        self.emit_edit(&platform_id, text);
    }

    /// Delete the status message and stop tracking it. If the native id is not
    /// known yet, the deletion is deferred until it arrives.
    fn finish(self) {
        let platform_id = {
            let mut map = PENDING_STATUS.lock().unwrap();
            let Some(state) = map.get_mut(&self.local_id) else {
                return;
            };
            match &state.platform_id {
                Some(id) => id.clone(),
                None => {
                    state.pending_text = None;
                    state.pending_delete = true;
                    return;
                }
            }
        };
        PENDING_STATUS.lock().unwrap().remove(&self.local_id);
        self.emit_delete(&platform_id);
    }

    fn emit_edit(&self, platform_id: &str, text: String) {
        let mut event = Event::message_edit("PayloadExtractBot", platform_id, text, None);
        if let Some(message) = event.message.as_mut() {
            message.to = self.to.clone();
        }
        event.receiver = self.receiver.clone();
        context::bot().emit_event(event);
    }

    fn emit_delete(&self, platform_id: &str) {
        let mut event = Event::message_delete("PayloadExtractBot", platform_id);
        if let Some(message) = event.message.as_mut() {
            message.to = self.to.clone();
        }
        event.receiver = self.receiver.clone();
        context::bot().emit_event(event);
    }
}

/// Record the native id for a status message and flush any update/delete that
/// was requested before the id was known. Called from `on_event` when the
/// adapter echoes back a `MessageSent` for our local status id.
fn resolve_status_message(local_id: &str, platform_id: &str) {
    let (handle, action) = {
        let mut map = PENDING_STATUS.lock().unwrap();
        let Some(state) = map.get_mut(local_id) else {
            return;
        };
        state.platform_id = Some(platform_id.to_string());
        let handle = StatusHandle {
            local_id: local_id.to_string(),
            to: state.to.clone(),
            receiver: state.receiver.clone(),
        };
        if state.pending_delete {
            (handle, StatusAction::Delete)
        } else if let Some(text) = state.pending_text.take() {
            (handle, StatusAction::Edit(text))
        } else {
            return;
        }
    };

    match action {
        StatusAction::Delete => {
            PENDING_STATUS.lock().unwrap().remove(local_id);
            handle.emit_delete(platform_id);
        }
        StatusAction::Edit(text) => handle.emit_edit(platform_id, text),
    }
}

enum StatusAction {
    Edit(String),
    Delete,
}

fn next_status_id() -> String {
    let id = NEXT_STATUS_ID.fetch_add(1, Ordering::Relaxed);
    format!("{PLUGIN_NAME}:status:{id}")
}

fn emit_files_with_caption(
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

fn emit_file_with_caption(
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

fn emit_content(
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
            delete_after,
        }),
        sender: None,
        receiver: request.receiver.clone(),
    };
    context::bot().emit_event(event);
}

fn text_item(text: impl Into<String>, format: Option<TextFormat>) -> ContentItem {
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

fn register_cleanup_path(path: PathBuf) -> String {
    let id = next_cleanup_id();
    PENDING_CLEANUPS.lock().unwrap().insert(id.clone(), path);

    let cleanup_id = id.clone();
    spawn_task(async move {
        tokio::time::sleep(CLEANUP_DELAY).await;
        let Some(path) = PENDING_CLEANUPS.lock().unwrap().remove(&cleanup_id) else {
            return;
        };
        cleanup_path_now(path);
    });

    id
}

fn cleanup_registered_path(cleanup_id: &str) {
    let Some(path) = PENDING_CLEANUPS.lock().unwrap().remove(cleanup_id) else {
        return;
    };
    cleanup_path_now(path);
}

fn cleanup_path_now(path: PathBuf) {
    match fs::remove_dir_all(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            log::warn!("failed to clean up {}: {e}", path.display());
        }
    }
}

fn next_cleanup_id() -> String {
    let id = NEXT_CLEANUP_ID.fetch_add(1, Ordering::Relaxed);
    format!("{PLUGIN_NAME}:cleanup:{id}")
}

fn file_name(path: &Path) -> Option<String> {
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
