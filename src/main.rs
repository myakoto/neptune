//! Neptune — живой переводчик речи для созвонов.
//!
//! Точка входа: подхватывает конфигурацию и разбирает аргументы.
//! Без команды (или с командой `gui`) открывается окно; CLI-команды
//! выполняются на tokio-рантайме.

// В release-сборке — чистое GUI-приложение без консольного окна.
// В debug консоль остаётся: там виден вывод CLI-команд и ошибки.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
    config::load_env();
    let args = cli::Args::parse();
    match args.command {
        None | Some(cli::Command::Gui) => gui::run(),
        Some(command) => tokio::runtime::Runtime::new()?.block_on(cli::run_command(command)),
    }
}
