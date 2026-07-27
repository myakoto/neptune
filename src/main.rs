//! Neptune — живой переводчик речи для созвонов.
//!
//! Точка входа: подхватывает `.env`, разбирает аргументы и передаёт
//! управление в [`cli::run`].

mod cli;
mod config;
mod stt;
mod translate;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    cli::run(cli::Args::parse()).await
}
