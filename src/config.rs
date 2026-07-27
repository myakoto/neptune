//! Загрузка конфигурации из переменных окружения (`.env` подхватывается в `main`).
//!
//! Ключи запрашиваются по отдельности: команде транскрипции не нужен ключ
//! Yandex, а команде перевода — ключ Deepgram.

use thiserror::Error;

/// Ошибки загрузки конфигурации.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Обязательная переменная окружения отсутствует или пуста.
    #[error("переменная окружения {0} не задана (добавь её в .env, см. .env.example)")]
    MissingVar(&'static str),
}

/// Ключ Deepgram API из `DEEPGRAM_API_KEY`.
pub fn deepgram_api_key() -> Result<String, ConfigError> {
    required("DEEPGRAM_API_KEY")
}

/// Ключ Yandex Cloud Translate API из `YANDEX_API_KEY`.
pub fn yandex_api_key() -> Result<String, ConfigError> {
    required("YANDEX_API_KEY")
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    non_empty(std::env::var(name).ok()).ok_or(ConfigError::MissingVar(name))
}

/// Пустые строки и строки из пробелов считаются отсутствующим значением.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_passes_real_values() {
        assert_eq!(non_empty(Some("key-123".into())), Some("key-123".into()));
    }

    #[test]
    fn non_empty_rejects_missing_and_blank() {
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some(String::new())), None);
        assert_eq!(non_empty(Some("   ".into())), None);
    }

    #[test]
    fn missing_var_error_names_the_variable() {
        let message = ConfigError::MissingVar("DEEPGRAM_API_KEY").to_string();
        assert!(message.contains("DEEPGRAM_API_KEY"));
    }
}
