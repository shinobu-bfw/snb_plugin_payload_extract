//! A Shinobu plugin that extracts, lists, inspects, and KernelSU-patches
//! Android partitions from a `payload.bin` / OTA URL.
//!
//! Results are emitted back through the bot's adapter (e.g. the Telegram
//! adapter). Patching shells out to `ksud`, downloaded on demand into the
//! plugin's data directory; it runs on Linux, Android, macOS, and Windows
//! (x86_64).

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use snb_core::command::CommandContext;
use snb_core::context::{self, BotContext};
use snb_core::event::{Event, EventType};
use snb_core::plugin::{PluginType, SnbPlugin, Version};
use snb_macros::{command, plugin};

mod auth;
mod cleanup;
mod config;
mod output;
mod patch_boot;
mod payload;
mod status;
mod tool;
mod utils;

use auth::is_admin;
use cleanup::{cleanup_path_now, cleanup_registered_path, register_cleanup_path};
use output::{
    dump_caption_markdown, emit_file_with_caption, emit_files_with_caption, emit_html_blockquote,
    emit_status_text, emit_temporary_text, emit_text, file_name, patch_caption_markdown,
};
use status::{
    StatusHandle, delete_status_when_sent, finish_status_on_sent, resolve_status_message,
};

const PLUGIN_NAME: &str = "PayloadExtract";
const CLEANUP_DELAY: Duration = Duration::from_secs(30 * 60);
const HINT_DELETE_DELAY: Duration = Duration::from_secs(10);

struct State {
    cfg: Arc<config::Config>,
    tm: Arc<tool::ToolManager>,
}

#[derive(Clone)]
struct CommandRequest {
    args: String,
    to: Option<String>,
    reply_to: Option<String>,
    receiver: Option<String>,
    from: Option<String>,
    is_admin: bool,
}

#[derive(Clone, Copy)]
enum CommandKind {
    Dump,
    List,
    Meta,
    Patch,
    Update,
    Status,
}

static STATE: RwLock<Option<Arc<State>>> = RwLock::new(None);

#[plugin]
struct PayloadExtract;

impl SnbPlugin for PayloadExtract {
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
        context::set_plugin(self.name());

        match init_state() {
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
        finish_status_on_sent(local_id);
        if let Some(platform_id) = message.id.as_deref() {
            resolve_status_message(local_id, platform_id);
        }
    }
}

fn init_state() -> anyhow::Result<State> {
    let cfg = Arc::new(load_plugin_config()?);
    let bin_root = context::plugin().data_dir().join("bin");
    let tm = Arc::new(tool::ToolManager::try_with_bin_root(bin_root)?);
    Ok(State { cfg, tm })
}

fn load_plugin_config() -> anyhow::Result<config::Config> {
    match context::plugin().load_config(Path::new("config.toml")) {
        Ok(content) => toml::from_str(&content).context("failed to parse PayloadExtract config"),
        Err(_) => {
            let default = config::Config::default();
            let content =
                toml::to_string_pretty(&default).context("failed to render default config")?;
            context::plugin()
                .write_config(Path::new("config.toml"), &content)
                .context("failed to write default PayloadExtract config")?;
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
        .context("PayloadExtract is not initialized")
}

#[command(name = "dump", aliases = ["dumper"], description = "Dump partition(s) from a payload URL")]
fn dump(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Dump)
}

#[command(name = "list", description = "List partition info of a payload URL")]
fn list(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::List)
}

#[command(name = "meta", aliases = ["metadata"], description = "Show OTA metadata from the OTA zip")]
fn meta(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Meta)
}

#[command(name = "patch", description = "Patch a boot partition with KernelSU")]
fn patch(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Patch)
}

#[command(name = "update", description = "Update ksud to the latest version")]
fn update(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Update)
}

#[command(name = "status", description = "Show current bot status")]
fn status(ctx: &CommandContext) -> anyhow::Result<()> {
    run_command(ctx, CommandKind::Status)
}

fn run_command(ctx: &CommandContext, kind: CommandKind) -> anyhow::Result<()> {
    let request = CommandRequest::from_context(ctx);
    spawn_task(async move {
        if let Err(e) = execute_command(kind, request.clone()).await {
            log::error!("PayloadExtract command failed: {e:#}");
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
            to: msg.map(|m| m.chat_id().to_string()),
            reply_to: msg.and_then(|m| m.id.clone()),
            receiver: ctx.event.reply_plugin.clone(),
            from: msg.and_then(|m| m.sender_id().map(str::to_string)),
            is_admin: msg.is_some_and(|m| m.is_admin),
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
        match patch_boot::patch_boot(url, partition.clone(), kmi, state.tm.clone(), progress).await
        {
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
        cleanup_id.clone(),
        patch_caption_markdown(&patched),
        patched.path().to_path_buf(),
        file_name(patched.path()),
    );
    match cleanup_id {
        Some(id) => delete_status_when_sent(id, status),
        None => status.finish(),
    }
    Ok(())
}

async fn update_command(request: CommandRequest) -> anyhow::Result<()> {
    let state = state()?;
    if !is_admin(&request, &state.cfg) {
        return Ok(());
    }

    emit_status_text(&request, "Updating tools...");
    match state.tm.update().await {
        Ok(tool::UpdateOutcome::Updated(tag)) => {
            emit_temporary_text(&request, format!("Tools updated to {tag}!"));
        }
        Ok(tool::UpdateOutcome::AlreadyLatest(tag)) => {
            emit_temporary_text(&request, format!("Tools already latest ({tag})."));
        }
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
        .filter(|name| {
            !supported
                .iter()
                .any(|pattern| matches_pattern(pattern, name))
        })
        .map(ToOwned::to_owned)
        .collect()
}

/// Match a partition `name` against a whitelist `pattern` that may contain glob
/// wildcards: `*` matches any (possibly empty) run of characters and `?` matches
/// exactly one. A pattern with no wildcards is an exact, case-sensitive match,
/// so plain entries like `boot` keep their original meaning.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    // Backtrack point: the index just after the most recent `*`, and how much of
    // `name` it has consumed so far. `None` means no `*` is open yet.
    let (mut star_pi, mut star_ni): (Option<usize>, usize) = (None, 0);

    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_pi = Some(pi);
            star_ni = ni;
            pi += 1;
        } else if let Some(sp) = star_pi {
            // Mismatch under an open `*`: let it swallow one more character.
            pi = sp + 1;
            star_ni += 1;
            ni = star_ni;
        } else {
            return false;
        }
    }

    // Any leftover pattern must be all `*` to match the empty remainder.
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

#[cfg(test)]
#[path = "../tests/unit/auth_tests.rs"]
mod auth_tests;

#[cfg(test)]
#[path = "../tests/unit/partition_tests.rs"]
mod partition_tests;
