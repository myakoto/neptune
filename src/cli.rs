//! Разбор аргументов командной строки и запуск команд.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::config;
use crate::stt::DeepgramClient;
use crate::translate::YandexTranslator;

/// Живой переводчик речи для созвонов (EN ⇄ RU).
#[derive(Parser)]
#[command(name = "neptune", version, about)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Перевести текст (по умолчанию с русского на английский)
    Translate {
        /// Текст для перевода
        text: String,
        /// Код исходного языка
        #[arg(long, default_value = "ru")]
        from: String,
        /// Код целевого языка
        #[arg(long, default_value = "en")]
        to: String,
    },
    /// Распознать WAV-файл через Deepgram
    Transcribe {
        /// Путь к WAV-файлу
        path: PathBuf,
        /// Принудительный язык (без флага — автоопределение)
        #[arg(long)]
        language: Option<String>,
    },
}

/// Выполняет команду, выбранную пользователем.
///
/// # Errors
/// Возвращает ошибку конфигурации, сети или API соответствующего сервиса.
pub async fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Translate { text, from, to } => {
            let translator = YandexTranslator::new(config::yandex_api_key()?);
            let translated = translator.translate(&text, &from, &to).await?;
            println!("{translated}");
        }
        Command::Transcribe { path, language } => {
            let client = DeepgramClient::new(config::deepgram_api_key()?);
            let wav = std::fs::read(&path)
                .with_context(|| format!("не удалось прочитать файл {}", path.display()))?;
            let transcript = client.transcribe_wav(wav, language.as_deref()).await?;
            println!("[{}] {}", transcript.language, transcript.text);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn translate_defaults_to_ru_en() {
        let args = Args::parse_from(["neptune", "translate", "привет"]);
        match args.command {
            Command::Translate { text, from, to } => {
                assert_eq!(text, "привет");
                assert_eq!(from, "ru");
                assert_eq!(to, "en");
            }
            Command::Transcribe { .. } => panic!("ожидалась команда translate"),
        }
    }
}
