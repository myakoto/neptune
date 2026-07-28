//! Команда `listen`: микрофон → Deepgram live → консоль (+ перевод финалов).
//!
//! Промежуточные гипотезы рисуются поверх одной строки, финалы печатаются
//! с меткой спикера; если задан `YANDEX_API_KEY`, под финалом — перевод.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::audio::{
    AudioCapture, CaptureSource, Resampler, TARGET_SAMPLE_RATE, downmix, parse_wav,
};
use crate::config;
use crate::stt::live::{self, LiveEvent, LiveOptions, LiveReceiver};
use crate::translate::YandexTranslator;

const KEEPALIVE_PERIOD: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const REPLAY_CHUNK: Duration = Duration::from_millis(100);

/// Источник аудио: живое устройство или реплей WAV-файла.
enum AudioFeed {
    Device(AudioCapture),
    Replay {
        sample_rate: u32,
        channels: u16,
        chunks: mpsc::UnboundedReceiver<Vec<f32>>,
    },
}

impl AudioFeed {
    fn sample_rate(&self) -> u32 {
        match self {
            Self::Device(capture) => capture.sample_rate,
            Self::Replay { sample_rate, .. } => *sample_rate,
        }
    }

    fn channels(&self) -> u16 {
        match self {
            Self::Device(capture) => capture.channels,
            Self::Replay { channels, .. } => *channels,
        }
    }

    async fn recv(&mut self) -> Option<Vec<f32>> {
        match self {
            Self::Device(capture) => capture.chunks.recv().await,
            Self::Replay { chunks, .. } => chunks.recv().await,
        }
    }
}

/// Читает WAV и отдаёт его чанками в реальном темпе, как микрофон.
fn start_replay(path: &PathBuf) -> Result<AudioFeed> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("не удалось прочитать файл {}", path.display()))?;
    let audio = parse_wav(&bytes)?;
    let (sample_rate, channels) = (audio.sample_rate, audio.channels);
    let frames_per_chunk =
        usize::try_from(sample_rate / 10).unwrap_or(1_600).max(1) * usize::from(channels);

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for chunk in audio.samples.chunks(frames_per_chunk) {
            if tx.send(chunk.to_vec()).is_err() {
                return;
            }
            tokio::time::sleep(REPLAY_CHUNK).await;
        }
        // tx дропается — поток чанков закончился, цикл listen завершится сам.
    });
    Ok(AudioFeed::Replay {
        sample_rate,
        channels,
        chunks: rx,
    })
}

/// Запускает живое распознавание с выбранного источника (или из WAV) до Ctrl+C.
///
/// # Errors
/// Возвращает ошибку конфигурации, аудиоустройства или соединения.
pub async fn run(language: String, source: CaptureSource, wav: Option<PathBuf>) -> Result<()> {
    let api_key = config::deepgram_api_key()?;
    let translator = config::yandex_api_key().ok().map(YandexTranslator::new);
    if translator.is_none() {
        eprintln!("YANDEX_API_KEY не задан — показываю только распознавание, без перевода");
    }

    let mut feed = match &wav {
        Some(path) => start_replay(path)?,
        None => AudioFeed::Device(
            AudioCapture::start(source).context("не удалось запустить захват звука")?,
        ),
    };
    let mut resampler = Resampler::new(feed.sample_rate(), TARGET_SAMPLE_RATE);
    let options = LiveOptions {
        language: language.clone(),
        diarize: true,
        sample_rate: TARGET_SAMPLE_RATE,
    };
    let (mut sender, receiver) = live::connect(&api_key, &options).await?;
    let source_name = wav.as_ref().map_or_else(
        || match source {
            CaptureSource::Mic => "микрофон".to_owned(),
            CaptureSource::Loopback => "loopback (звук системы)".to_owned(),
        },
        |p| p.display().to_string(),
    );
    println!(
        "Источник: {source_name} ({} Hz, {} кан. → 16 kHz mono). Ctrl+C — стоп.",
        feed.sample_rate(),
        feed.channels()
    );

    let printer = tokio::spawn(print_events(receiver, translator, language));
    let mut keepalive = tokio::time::interval(KEEPALIVE_PERIOD);
    let channels = feed.channels();

    loop {
        tokio::select! {
            chunk = feed.recv() => match chunk {
                Some(samples) => {
                    let pcm = resampler.process(&downmix(&samples, channels));
                    if !pcm.is_empty() {
                        sender.send_audio(&pcm).await?;
                    }
                }
                None => break,
            },
            _ = keepalive.tick() => sender.keep_alive().await?,
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    drop(feed); // останавливаем захват до финализации
    sender.close_stream().await.ok();
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, printer).await;
    println!("\nГотово.");
    Ok(())
}

/// Печатает события распознавания; живёт, пока сервер не закроет поток.
async fn print_events(
    mut receiver: LiveReceiver,
    translator: Option<YandexTranslator>,
    source_language: String,
) {
    let target = target_language(&source_language);
    let mut interim_width = 0;

    while let Some(event) = receiver.next_event().await {
        match event {
            LiveEvent::Interim(text) => interim_width = draw_interim(&text, interim_width),
            LiveEvent::Final { text, speaker } => {
                clear_line(interim_width);
                interim_width = 0;
                let label = speaker.map_or_else(String::new, |s| format!("[S{s}] "));
                println!("{label}{text}");
                if let Some(translator) = &translator {
                    show_translation(translator, &text, &source_language, target).await;
                }
            }
            LiveEvent::Ignored => {}
        }
    }
}

async fn show_translation(translator: &YandexTranslator, text: &str, from: &str, to: &str) {
    match translator.translate(text, from, to).await {
        Ok(translated) => println!("   → {translated}"),
        Err(error) => eprintln!("   перевод не удался: {error}"),
    }
}

/// Рисует промежуточную гипотезу поверх текущей строки, возвращает её ширину.
fn draw_interim(text: &str, previous_width: usize) -> usize {
    let line = format!("… {text}");
    let width = line.chars().count();
    print!(
        "\r{line}{}",
        " ".repeat(previous_width.saturating_sub(width))
    );
    let _ = std::io::stdout().flush();
    width
}

fn clear_line(width: usize) {
    if width > 0 {
        print!("\r{}\r", " ".repeat(width));
        let _ = std::io::stdout().flush();
    }
}

/// Направление перевода: русский переводим на английский, остальное — на русский.
fn target_language(source: &str) -> &'static str {
    if source.starts_with("ru") { "en" } else { "ru" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_translates_to_english() {
        assert_eq!(target_language("ru"), "en");
    }

    #[test]
    fn other_languages_translate_to_russian() {
        assert_eq!(target_language("en"), "ru");
        assert_eq!(target_language("multi"), "ru");
    }
}
