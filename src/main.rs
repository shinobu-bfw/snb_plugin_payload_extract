mod cmd_impl;
mod commands;
mod config;
mod patch_boot;
mod payload;
mod tool;
mod utils;

use crate::commands::{Command, answer};
use crate::utils::log_message;
use anyhow::Result;
use log::{debug, info};
use std::time::Duration;
use teloxide::dispatching::{Dispatcher, HandlerExt, UpdateFilterExt};
use teloxide::prelude::{Bot, Update};
use teloxide::{dptree, net};

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = std::sync::Arc::new(config::load_config()?);
    pretty_env_logger::init();
    info!("Initializing tools");
    let tm = std::sync::Arc::new(tool::ToolManager::default());
    tm.init().await?;
    info!("Cleaning temp files");
    std::fs::remove_dir_all("tmp").ok();
    info!("Starting command bot...");
    let client = net::default_reqwest_settings().timeout(Duration::from_secs(120));
    let bot =
        Bot::with_client(cfg.token.clone(), client.build()?).set_api_url(cfg.api_url.parse()?);

    let handler = Update::filter_message()
        .branch(dptree::entry().filter_command::<Command>().endpoint(answer))
        .branch(dptree::endpoint(log_message));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![tm, cfg])
        .default_handler(|upd| async move {
            debug!("Ignoring unmatched update: {:?}", upd);
        })
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}
