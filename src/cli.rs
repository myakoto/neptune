//! Разбор аргументов командной строки и запуск команд.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::audio::CaptureSource;
use crate::config;
use crate::stt::DeepgramClient;
use crate::translate::YandexTranslator;

/// Источник звука для команды `listen`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SourceArg {
    /// Микрофон (моя речь)
    Mic,
    /// Звук системы — то, что играет в наушниках (речь собеседников)
    Loopback,
}

impl From<SourceArg> for CaptureSource {
    fn from(source: SourceArg) -> Self {
        match source {
            SourceArg::Mic => Self::Mic,
            SourceArg::Loopback => Self::Loopback,
        }
    }
}

/// Живой переводчик речи для созвонов (EN ⇄ RU).
#[derive(Parser)]
#[command(name = "neptune", version, about)]
pub struct Args {
    /// Команда; без неё открывается GUI-окно.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Команды приложения.
#[derive(Subcommand)]
pub enum Command {
    /// Открыть GUI-окно (режим по умолчанию)
    Gui,
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
    /// Живое распознавание речи (Ctrl+C — стоп)
    Listen {
        /// Язык речи ("en", "ru" или "multi" для смешанной)
        #[arg(long, default_value = "en")]
        language: String,
        /// Источник звука: микрофон или loopback (звук созвона)
        #[arg(long, value_enum, default_value_t = SourceArg::Mic)]
        source: SourceArg,
        /// Прогнать WAV-файл вместо живого источника (отладка пайплайна)
        #[arg(long)]
        wav: Option<PathBuf>,
    },
}

/// Выполняет CLI-команду (GUI обрабатывается в `main` до этого вызова).
///
/// # Errors
/// Возвращает ошибку конфигурации, сети или API соответствующего сервиса.
pub async fn run_command(command: Command) -> Result<()> {
    match command {
        Command::Gui => unreachable!("gui запускается из main без tokio-рантайма"),
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
        Command::Listen {
            language,
            source,
            wav,
        } => {
            crate::listen::run(language, source.into(), wav).await?;
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
            Some(Command::Translate { text, from, to }) => {
                assert_eq!(text, "привет");
                assert_eq!(from, "ru");
                assert_eq!(to, "en");
            }
            _ => panic!("ожидалась команда translate"),
        }
    }

    #[test]
    fn no_command_means_gui() {
        let args = Args::parse_from(["neptune"]);
        assert!(args.command.is_none());
    }
}
