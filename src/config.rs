//! Конфигурация: ключи API из переменных окружения.
//!
//! `.env` подхватывается слоями (`load_env` в `main`), приоритет сверху вниз:
//! 1. текущая директория (удобно при разработке),
//! 2. директория рядом с exe,
//! 3. `%APPDATA%\neptune\.env` — туда пишет окно настроек GUI.
//!
//! Ключи запрашиваются по отдельности: команде транскрипции не нужен ключ
//! Yandex, а команде перевода — ключ Deepgram.

use std::path::PathBuf;

use thiserror::Error;

/// Ошибки загрузки конфигурации.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Обязательная переменная окружения отсутствует или пуста.
    #[error("переменная окружения {0} не задана")]
    MissingVar(&'static str),
}

/// Подхватывает `.env` из всех известных мест (существующие переменные
/// окружения не перезаписываются, ранний слой сильнее позднего).
pub fn load_env() {
    dotenvy::dotenv().ok();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dotenvy::from_path(dir.join(".env")).ok();
    }
    if let Some(path) = appdata_env_path() {
        dotenvy::from_path(path).ok();
    }
}

/// Путь к пользовательскому конфигу: `%APPDATA%\neptune\.env`.
#[must_use]
pub fn appdata_env_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("neptune").join(".env"))
}

/// Сохраняет ключи в `%APPDATA%\neptune\.env`; возвращает путь к файлу.
/// Пустые ключи не записываются. Подхватятся при следующем запуске.
///
/// # Errors
/// Возвращает ошибку, если `%APPDATA%` недоступен или файл не записался.
pub fn save_keys(deepgram: &str, yandex: &str) -> anyhow::Result<PathBuf> {
    let path = appdata_env_path()
        .ok_or_else(|| anyhow::anyhow!("переменная окружения APPDATA недоступна"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, render_env(deepgram, yandex))?;
    Ok(path)
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

/// Собирает содержимое `.env` из непустых ключей.
fn render_env(deepgram: &str, yandex: &str) -> String {
    let mut out = String::new();
    for (name, value) in [("DEEPGRAM_API_KEY", deepgram), ("YANDEX_API_KEY", yandex)] {
        let value = value.trim();
        if !value.is_empty() {
            out.push_str(name);
            out.push('=');
            out.push_str(value);
            out.push('\n');
        }
    }
    out
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

    #[test]
    fn render_env_writes_both_keys() {
        let content = render_env("dg-key", "ya-key");
        assert_eq!(content, "DEEPGRAM_API_KEY=dg-key\nYANDEX_API_KEY=ya-key\n");
    }

    #[test]
    fn render_env_skips_blank_keys() {
        assert_eq!(render_env("dg-key", "  "), "DEEPGRAM_API_KEY=dg-key\n");
        assert_eq!(render_env("", ""), "");
    }
}
