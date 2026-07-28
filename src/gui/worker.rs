//! Фоновый воркер GUI: держит оба пайплайна на tokio-рантайме.
//!
//! Субтитры: loopback → Deepgram live (en) → Yandex → события в окно,
//! с автопереподключением. Push-to-talk: микрофон → Deepgram live (ru) →
//! Yandex (ru→en) → буфер обмена + событие с результатом.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::audio::{AudioCapture, CaptureSource, Resampler, TARGET_SAMPLE_RATE, downmix};
use crate::config;
use crate::gui::messages::{SessionStats, UiCommand, UiEvent};
use crate::stt::live::{self, LiveEvent, LiveOptions, LiveReceiver, LiveSender};
use crate::translate::YandexTranslator;

const KEEPALIVE_PERIOD: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const PTT_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Отправитель событий в окно: каждое событие будит перерисовку.
#[derive(Clone)]
pub struct EventSender {
    tx: mpsc::UnboundedSender<UiEvent>,
    ctx: eframe::egui::Context,
}

impl EventSender {
    /// Создаёт отправителя, привязанного к egui-контексту.
    #[must_use]
    pub fn new(tx: mpsc::UnboundedSender<UiEvent>, ctx: eframe::egui::Context) -> Self {
        Self { tx, ctx }
    }

    fn send(&self, event: UiEvent) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }

    fn error(&self, message: impl Into<String>) {
        self.send(UiEvent::Error(message.into()));
    }
}

/// Главный цикл воркера: обрабатывает команды окна.
pub async fn run(mut commands: mpsc::UnboundedReceiver<UiCommand>, events: EventSender) {
    let Ok(api_key) = config::deepgram_api_key() else {
        events.error("Нет ключа Deepgram — открой ⚙ настройки, вставь ключ и перезапусти");
        return;
    };
    let translator = config::yandex_api_key()
        .ok()
        .map(|key| Arc::new(YandexTranslator::new(key)));
    if translator.is_none() {
        events.error("Нет ключа Yandex — распознавание без перевода (⚙ настройки)");
    }
    let stats = SessionStats::new(translator.is_some());

    tokio::spawn(check_update_task(events.clone()));

    let mut subtitles: Option<JoinHandle<()>> = None;
    let mut ptt_stop: Option<oneshot::Sender<()>> = None;

    while let Some(command) = commands.recv().await {
        match command {
            UiCommand::SetListening(true) => {
                if subtitles.is_none() {
                    subtitles = Some(tokio::spawn(subtitle_task(
                        api_key.clone(),
                        translator.clone(),
                        Arc::clone(&stats),
                        events.clone(),
                    )));
                    events.send(UiEvent::Listening(true));
                }
            }
            UiCommand::SetListening(false) => {
                if let Some(handle) = subtitles.take() {
                    handle.abort();
                }
                stats.set_deepgram_ok(false);
                events.send(UiEvent::Listening(false));
                events.send(UiEvent::Status(stats.snapshot()));
            }
            UiCommand::PttPress => {
                if ptt_stop.is_none() {
                    let (stop_tx, stop_rx) = oneshot::channel();
                    ptt_stop = Some(stop_tx);
                    tokio::spawn(ptt_task(
                        api_key.clone(),
                        translator.clone(),
                        Arc::clone(&stats),
                        events.clone(),
                        stop_rx,
                    ));
                }
            }
            UiCommand::PttRelease => {
                if let Some(stop) = ptt_stop.take() {
                    let _ = stop.send(());
                }
            }
            UiCommand::ApplyUpdate => {
                tokio::spawn(apply_update_task(events.clone()));
            }
        }
    }
}

/// Разовая проверка обновлений при старте (blocking-код — в отдельном треде).
async fn check_update_task(events: EventSender) {
    match tokio::task::spawn_blocking(crate::update::check).await {
        Ok(Ok(Some(version))) => events.send(UiEvent::UpdateAvailable(version)),
        Ok(Ok(None)) => {}
        Ok(Err(error)) => events.error(format!("проверка обновлений: {error}")),
        Err(_) => events.error("проверка обновлений прервалась"),
    }
}

/// Скачивание и установка обновления по запросу из окна.
async fn apply_update_task(events: EventSender) {
    match tokio::task::spawn_blocking(crate::update::apply).await {
        Ok(Ok(version)) => events.send(UiEvent::UpdateApplied(version)),
        Ok(Err(error)) => events.send(UiEvent::UpdateFailed(error.to_string())),
        Err(_) => events.send(UiEvent::UpdateFailed("обновление прервалось".into())),
    }
}

/// Субтитры: бесконечный цикл сессий с переподключением (снимается abort'ом).
async fn subtitle_task(
    api_key: String,
    translator: Option<Arc<YandexTranslator>>,
    stats: Arc<SessionStats>,
    events: EventSender,
) {
    loop {
        if let Err(error) = subtitle_session(&api_key, translator.clone(), &stats, &events).await {
            stats.set_deepgram_ok(false);
            events.error(format!("субтитры: {error}; переподключаюсь…"));
            events.send(UiEvent::Status(stats.snapshot()));
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Одна сессия субтитров: захват loopback и стрим до обрыва соединения.
async fn subtitle_session(
    api_key: &str,
    translator: Option<Arc<YandexTranslator>>,
    stats: &Arc<SessionStats>,
    events: &EventSender,
) -> anyhow::Result<()> {
    let mut capture = AudioCapture::start(CaptureSource::Loopback)?;
    let mut resampler = Resampler::new(capture.sample_rate, TARGET_SAMPLE_RATE);
    let channels = capture.channels;

    let options = LiveOptions {
        language: "en".to_owned(),
        diarize: true,
        sample_rate: TARGET_SAMPLE_RATE,
    };
    let (mut sender, receiver) = live::connect(api_key, &options).await?;
    stats.set_deepgram_ok(true);
    events.send(UiEvent::Status(stats.snapshot()));

    let mut reader = tokio::spawn(subtitle_reader(
        receiver,
        translator,
        Arc::clone(stats),
        events.clone(),
    ));
    let mut keepalive = tokio::time::interval(KEEPALIVE_PERIOD);

    let result = loop {
        tokio::select! {
            chunk = capture.chunks.recv() => match chunk {
                Some(samples) => {
                    if let Err(error) =
                        pump_audio(&mut sender, &mut resampler, &samples, channels, stats).await
                    {
                        break Err(error);
                    }
                }
                None => break Err(anyhow::anyhow!("аудиоустройство остановилось")),
            },
            _ = keepalive.tick() => {
                if let Err(error) = sender.keep_alive().await {
                    break Err(error.into());
                }
            }
            _ = &mut reader => break Err(anyhow::anyhow!("сервер закрыл соединение")),
        }
    };
    reader.abort();
    result
}

/// Прогоняет чанк через ресемплер и отправляет в Deepgram, ведя счётчики.
async fn pump_audio(
    sender: &mut LiveSender,
    resampler: &mut Resampler,
    samples: &[f32],
    channels: u16,
    stats: &Arc<SessionStats>,
) -> anyhow::Result<()> {
    let pcm = resampler.process(&downmix(samples, channels));
    if pcm.is_empty() {
        return Ok(());
    }
    stats.add_audio_samples(pcm.len(), TARGET_SAMPLE_RATE);
    sender.send_audio(&pcm).await?;
    Ok(())
}

/// Читает события Deepgram, переводит финалы и шлёт их в окно.
async fn subtitle_reader(
    mut receiver: LiveReceiver,
    translator: Option<Arc<YandexTranslator>>,
    stats: Arc<SessionStats>,
    events: EventSender,
) {
    while let Some(event) = receiver.next_event().await {
        match event {
            LiveEvent::Interim(text) => events.send(UiEvent::SubtitleInterim(text)),
            LiveEvent::Final { text, speaker } => {
                let translation =
                    translate_or_none(translator.as_deref(), &stats, &text, "en", "ru").await;
                events.send(UiEvent::SubtitleFinal {
                    speaker,
                    text,
                    translation,
                });
                events.send(UiEvent::Status(stats.snapshot()));
            }
            LiveEvent::Ignored => {}
        }
    }
}

/// Переводит текст, если переводчик настроен; ошибки превращает в `None`.
async fn translate_or_none(
    translator: Option<&YandexTranslator>,
    stats: &Arc<SessionStats>,
    text: &str,
    from: &str,
    to: &str,
) -> Option<String> {
    let translator = translator?;
    let result = translator.translate(text, from, to).await.ok();
    stats.add_translation(text.chars().count(), result.is_some());
    result
}

/// Push-to-talk сессия: пишем микрофон, пока не придёт сигнал отпускания.
async fn ptt_task(
    api_key: String,
    translator: Option<Arc<YandexTranslator>>,
    stats: Arc<SessionStats>,
    events: EventSender,
    mut stop: oneshot::Receiver<()>,
) {
    match ptt_session(&api_key, translator, &stats, &events, &mut stop).await {
        Ok(()) => {}
        Err(error) => events.error(format!("push-to-talk: {error}")),
    }
    events.send(UiEvent::Status(stats.snapshot()));
}

async fn ptt_session(
    api_key: &str,
    translator: Option<Arc<YandexTranslator>>,
    stats: &Arc<SessionStats>,
    events: &EventSender,
    stop: &mut oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let mut capture = AudioCapture::start(CaptureSource::Mic)?;
    let mut resampler = Resampler::new(capture.sample_rate, TARGET_SAMPLE_RATE);
    let channels = capture.channels;

    let options = LiveOptions {
        language: "ru".to_owned(),
        diarize: false,
        sample_rate: TARGET_SAMPLE_RATE,
    };
    let (mut sender, mut receiver) = live::connect(api_key, &options).await?;

    let mut finals: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            chunk = capture.chunks.recv() => match chunk {
                Some(samples) => {
                    pump_audio(&mut sender, &mut resampler, &samples, channels, stats).await?;
                }
                None => break,
            },
            event = receiver.next_event() => match event {
                Some(LiveEvent::Interim(text)) => events.send(UiEvent::PttInterim(text)),
                Some(LiveEvent::Final { text, .. }) => finals.push(text),
                Some(LiveEvent::Ignored) => {}
                None => anyhow::bail!("сервер закрыл соединение"),
            },
            _ = &mut *stop => break,
        }
    }

    drop(capture);
    sender.close_stream().await.ok();
    collect_remaining_finals(&mut receiver, &mut finals, events).await;

    let recognized = finals.join(" ");
    if recognized.is_empty() {
        events.send(UiEvent::PttDone {
            recognized,
            translated: None,
            copied: false,
        });
        return Ok(());
    }
    let translated = translate_or_none(translator.as_deref(), stats, &recognized, "ru", "en").await;
    let copied = translated.as_deref().is_some_and(copy_to_clipboard);
    events.send(UiEvent::PttDone {
        recognized,
        translated,
        copied,
    });
    Ok(())
}

/// После `CloseStream` дожидается финализации остатка речи.
async fn collect_remaining_finals(
    receiver: &mut LiveReceiver,
    finals: &mut Vec<String>,
    events: &EventSender,
) {
    let drain = async {
        while let Some(event) = receiver.next_event().await {
            match event {
                LiveEvent::Final { text, .. } => finals.push(text),
                LiveEvent::Interim(text) => events.send(UiEvent::PttInterim(text)),
                LiveEvent::Ignored => {}
            }
        }
    };
    let _ = tokio::time::timeout(PTT_DRAIN_TIMEOUT, drain).await;
}

/// Кладёт текст в буфер обмена; `false` — не получилось.
fn copy_to_clipboard(text: &str) -> bool {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_owned()))
        .is_ok()
}
