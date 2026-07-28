//! Протокол между окном (UI-поток) и воркером (tokio-рантайм):
//! команды вниз, события вверх. Плюс счётчики сессии и оценка стоимости.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Примерная цена Deepgram Nova-3 (стриминг), $ за минуту аудио.
const DEEPGRAM_USD_PER_MINUTE: f64 = 0.0092;
/// Примерная цена Yandex Translate, $ за миллион символов.
const YANDEX_USD_PER_MILLION_CHARS: f64 = 4.0;

/// Команды из окна воркеру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    /// Включить/выключить субтитры (loopback-пайплайн).
    SetListening(bool),
    /// Начать запись push-to-talk с микрофона.
    PttPress,
    /// Закончить запись и получить перевод.
    PttRelease,
    /// Скачать и установить доступное обновление.
    ApplyUpdate,
}

/// События воркера для окна.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    /// Промежуточная гипотеза входящей речи.
    SubtitleInterim(String),
    /// Финальная реплика собеседника (перевод — если есть ключ Yandex).
    SubtitleFinal {
        /// Номер спикера от диаризации.
        speaker: Option<u32>,
        /// Оригинал (EN).
        text: String,
        /// Перевод (RU), если переводчик доступен.
        translation: Option<String>,
    },
    /// Субтитры включены/выключены (подтверждение от воркера).
    Listening(bool),
    /// Промежуточная гипотеза моей речи в PTT.
    PttInterim(String),
    /// Итог PTT-сессии.
    PttDone {
        /// Что расслышано по-русски.
        recognized: String,
        /// Перевод на английский, если переводчик доступен.
        translated: Option<String>,
        /// Удалось ли положить перевод в буфер обмена.
        copied: bool,
    },
    /// Свежие счётчики сессии.
    Status(StatusSnapshot),
    /// Ошибка для показа в статусе.
    Error(String),
    /// На GitHub есть релиз новее текущей версии.
    UpdateAvailable(String),
    /// Проверка прошла: стоит последняя версия, можно запускаться.
    UpdateUpToDate,
    /// Проверить не удалось (GitHub недоступен) — запускаемся как есть.
    UpdateCheckFailed(String),
    /// Обновление скачано и установлено, нужен перезапуск.
    UpdateApplied(String),
    /// Обновление не удалось.
    UpdateFailed(String),
}

/// Снимок счётчиков для статус-строки.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatusSnapshot {
    /// Секунды аудио, отправленные в Deepgram.
    pub audio_seconds: f64,
    /// Символы, отправленные в Yandex Translate.
    pub translated_chars: u64,
    /// Deepgram-соединение живо.
    pub deepgram_ok: bool,
    /// Последний перевод прошёл успешно (None — ключа нет).
    pub yandex_ok: Option<bool>,
}

/// Общие счётчики сессии; обновляются из задач воркера.
#[derive(Debug, Default)]
pub struct SessionStats {
    /// Миллисекунды аудио, отправленные в Deepgram.
    audio_millis: AtomicU64,
    /// Символы, отправленные в переводчик.
    translated_chars: AtomicU64,
    /// Deepgram-соединение живо.
    deepgram_ok: AtomicBool,
    /// Последний перевод успешен.
    yandex_ok: AtomicBool,
    /// Переводчик вообще настроен.
    yandex_present: AtomicBool,
}

impl SessionStats {
    /// Создаёт счётчики; `yandex_present` — задан ли ключ переводчика.
    #[must_use]
    pub fn new(yandex_present: bool) -> Arc<Self> {
        let stats = Self::default();
        stats
            .yandex_present
            .store(yandex_present, Ordering::Relaxed);
        stats.yandex_ok.store(yandex_present, Ordering::Relaxed);
        Arc::new(stats)
    }

    /// Учитывает отправленные PCM-сэмплы (16 kHz mono).
    pub fn add_audio_samples(&self, samples: usize, sample_rate: u32) {
        let millis = samples as u64 * 1000 / u64::from(sample_rate.max(1));
        self.audio_millis.fetch_add(millis, Ordering::Relaxed);
    }

    /// Учитывает символы, ушедшие в переводчик, и результат вызова.
    pub fn add_translation(&self, chars: usize, ok: bool) {
        self.translated_chars
            .fetch_add(chars as u64, Ordering::Relaxed);
        self.yandex_ok.store(ok, Ordering::Relaxed);
    }

    /// Отмечает состояние соединения с Deepgram.
    pub fn set_deepgram_ok(&self, ok: bool) {
        self.deepgram_ok.store(ok, Ordering::Relaxed);
    }

    /// Текущий снимок для статус-строки.
    #[must_use]
    pub fn snapshot(&self) -> StatusSnapshot {
        #[allow(clippy::cast_precision_loss)] // миллисекунды сессии влезают в f64 с запасом
        let audio_seconds = self.audio_millis.load(Ordering::Relaxed) as f64 / 1000.0;
        StatusSnapshot {
            audio_seconds,
            translated_chars: self.translated_chars.load(Ordering::Relaxed),
            deepgram_ok: self.deepgram_ok.load(Ordering::Relaxed),
            yandex_ok: self
                .yandex_present
                .load(Ordering::Relaxed)
                .then(|| self.yandex_ok.load(Ordering::Relaxed)),
        }
    }
}

/// Примерная стоимость сессии в долларах.
#[must_use]
#[allow(clippy::cast_precision_loss)] // счётчики сессии влезают в f64 с запасом
pub fn estimate_cost_usd(snapshot: &StatusSnapshot) -> f64 {
    snapshot.audio_seconds / 60.0 * DEEPGRAM_USD_PER_MINUTE
        + snapshot.translated_chars as f64 / 1_000_000.0 * YANDEX_USD_PER_MILLION_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_accumulate_audio_and_chars() {
        let stats = SessionStats::new(true);
        stats.add_audio_samples(16_000, 16_000); // ровно секунда
        stats.add_audio_samples(8_000, 16_000); // ещё полсекунды
        stats.add_translation(120, true);
        let snap = stats.snapshot();
        assert!((snap.audio_seconds - 1.5).abs() < 1e-9);
        assert_eq!(snap.translated_chars, 120);
        assert_eq!(snap.yandex_ok, Some(true));
    }

    #[test]
    fn snapshot_hides_yandex_without_key() {
        let stats = SessionStats::new(false);
        assert_eq!(stats.snapshot().yandex_ok, None);
    }

    #[test]
    fn cost_counts_both_services() {
        let snapshot = StatusSnapshot {
            audio_seconds: 600.0, // 10 минут
            translated_chars: 500_000,
            deepgram_ok: true,
            yandex_ok: Some(true),
        };
        let cost = estimate_cost_usd(&snapshot);
        let expected = 10.0 * DEEPGRAM_USD_PER_MINUTE + 0.5 * YANDEX_USD_PER_MILLION_CHARS;
        assert!((cost - expected).abs() < 1e-9);
    }
}
