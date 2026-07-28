//! Neptune — живой переводчик речи для созвонов.
//!
//! Точка входа: подхватывает `.env` и разбирает аргументы. Без команды
//! (или с командой `gui`) открывается окно; CLI-команды выполняются
//! на tokio-рантайме.

mod audio;
mod cli;
mod config;
mod gui;
mod listen;
mod stt;
mod translate;
mod update;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = cli::Args::parse();
    match args.command {
        None | Some(cli::Command::Gui) => gui::run(),
        Some(command) => tokio::runtime::Runtime::new()?.block_on(cli::run_command(command)),
    }
}
