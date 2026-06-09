//! Shinobu compatibility layer for payload_extract_bot-rs.

use payload_extract_bot::{config, patch_boot, payload, tool, utils};

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use snb_core::command::CommandContext;
use snb_core::context::{self, BotContext};
use snb_core::event::{Event, EventType, TextFormat};
use snb_core::plugin::{PluginType, SnbPlugin, Version};
use snb_macros::{command, plugin};

#[path = "snb_compat/auth.rs"]
mod auth;
#[path = "snb_compat/cleanup.rs"]
mod cleanup;
#[path = "snb_compat/output.rs"]
mod output;
#[path = "snb_compat/status.rs"]
mod status;

use auth::is_admin;
use cleanup::{cleanup_path_now, cleanup_registered_path, register_cleanup_path};
use output::{
    dump_caption_markdown, emit_file_with_caption, emit_files_with_caption, emit_formatted_text,
    emit_html_blockquote, emit_status_text, emit_temporary_text, emit_text, file_name,
    patch_caption_markdown,
};
use status::{StatusHandle, delete_status_when_sent, finish_status_on_sent, resolve_status_message};

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
    Help,
}

static STATE: RwLock<Option<Arc<State>>> = RwLock::new(None);

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
        Ok(content) => toml::from_str(&content).context("failed to parse PayloadExtractBot config"),
        Err(_) => {
            let default = config::Config::default();
            let content =
                toml::to_string_pretty(&default).context("failed to render default config")?;
            context::plugin()
                .write_config(Path::new("config.toml"), &content)
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

#[cfg(test)]
#[path = "../tests/unit/snb_compat_tests.rs"]
mod snb_compat_tests;
