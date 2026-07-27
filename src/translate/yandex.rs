//! Клиент Yandex Cloud Translate v2 (REST).
//!
//! Аутентификация — API-ключом сервисного аккаунта: заголовок
//! `Authorization: Api-Key <key>`, `folder_id` при этом не нужен.

use serde::{Deserialize, Serialize};
use thiserror::Error;

const API_URL: &str = "https://translate.api.cloud.yandex.net/translate/v2/translate";

/// Ошибки клиента перевода.
#[derive(Debug, Error)]
pub enum TranslateError {
    /// Сетевая ошибка или ошибка разбора ответа.
    #[error("запрос к Yandex Translate не удался: {0}")]
    Http(#[from] reqwest::Error),
    /// API вернул неуспешный HTTP-статус.
    #[error("Yandex Translate вернул {status}: {body}")]
    Api {
        /// HTTP-статус ответа.
        status: u16,
        /// Тело ответа с описанием ошибки.
        body: String,
    },
    /// В успешном ответе не оказалось перевода.
    #[error("Yandex Translate вернул пустой ответ")]
    EmptyResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TranslateRequest<'a> {
    source_language_code: &'a str,
    target_language_code: &'a str,
    texts: [&'a str; 1],
}

#[derive(Deserialize)]
struct TranslateResponse {
    translations: Vec<Translation>,
}

#[derive(Deserialize)]
struct Translation {
    text: String,
}

/// Клиент Yandex Cloud Translate.
pub struct YandexTranslator {
    client: reqwest::Client,
    api_key: String,
}

impl YandexTranslator {
    /// Создаёт клиент с указанным API-ключом.
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
        }
    }

    /// Переводит `text` с языка `from` на язык `to` (коды ISO 639-1: "en", "ru").
    ///
    /// # Errors
    /// Возвращает [`TranslateError`] при сетевой ошибке, неуспешном статусе
    /// или пустом ответе API.
    pub async fn translate(
        &self,
        text: &str,
        from: &str,
        to: &str,
    ) -> Result<String, TranslateError> {
        let request = TranslateRequest {
            source_language_code: from,
            target_language_code: to,
            texts: [text],
        };
        let response = self
            .client
            .post(API_URL)
            .header("Authorization", format!("Api-Key {}", self.api_key))
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(TranslateError::Api {
                status: status.as_u16(),
                body,
            });
        }
        first_translation(&response.text().await?)
    }
}

/// Достаёт первый перевод из JSON-ответа API.
fn first_translation(body: &str) -> Result<String, TranslateError> {
    let parsed: TranslateResponse =
        serde_json::from_str(body).map_err(|_| TranslateError::EmptyResponse)?;
    parsed
        .translations
        .into_iter()
        .next()
        .map(|t| t.text)
        .ok_or(TranslateError::EmptyResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_to_camel_case() {
        let request = TranslateRequest {
            source_language_code: "ru",
            target_language_code: "en",
            texts: ["привет"],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["sourceLanguageCode"], "ru");
        assert_eq!(json["targetLanguageCode"], "en");
        assert_eq!(json["texts"][0], "привет");
    }

    #[test]
    fn first_translation_parses_api_response() {
        let body = r#"{"translations":[{"text":"hello","detectedLanguageCode":"ru"}]}"#;
        assert_eq!(first_translation(body).unwrap(), "hello");
    }

    #[test]
    fn first_translation_rejects_empty_list() {
        assert!(matches!(
            first_translation(r#"{"translations":[]}"#),
            Err(TranslateError::EmptyResponse)
        ));
    }

    #[test]
    fn first_translation_rejects_malformed_json() {
        assert!(matches!(
            first_translation("not json"),
            Err(TranslateError::EmptyResponse)
        ));
    }
}
