//! Клиент Deepgram prerecorded API (REST) для готовых аудиофайлов.
//!
//! Это первый шаг пайплайна и проверка ключа; стриминговый WebSocket-клиент
//! для живого звука появится отдельным модулем.

use serde::Deserialize;
use thiserror::Error;

const API_URL: &str = "https://api.deepgram.com/v1/listen";
const MODEL: &str = "nova-3";

/// Ошибки клиента распознавания.
#[derive(Debug, Error)]
pub enum SttError {
    /// Сетевая ошибка или ошибка разбора ответа.
    #[error("запрос к Deepgram не удался: {0}")]
    Http(#[from] reqwest::Error),
    /// API вернул неуспешный HTTP-статус.
    #[error("Deepgram вернул {status}: {body}")]
    Api {
        /// HTTP-статус ответа.
        status: u16,
        /// Тело ответа с описанием ошибки.
        body: String,
    },
    /// В ответе не оказалось транскрипта.
    #[error("Deepgram вернул ответ без транскрипта")]
    EmptyResponse,
}

/// Результат распознавания.
#[derive(Debug, PartialEq, Eq)]
pub struct Transcript {
    /// Распознанный текст с пунктуацией (`smart_format`).
    pub text: String,
    /// Код определённого языка ("en", "ru", …).
    pub language: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    results: Results,
}

#[derive(Deserialize)]
struct Results {
    channels: Vec<Channel>,
}

#[derive(Deserialize)]
struct Channel {
    alternatives: Vec<Alternative>,
    detected_language: Option<String>,
}

#[derive(Deserialize)]
struct Alternative {
    transcript: String,
}

/// Клиент Deepgram API.
pub struct DeepgramClient {
    client: reqwest::Client,
    api_key: String,
}

impl DeepgramClient {
    /// Создаёт клиент с указанным API-ключом.
    #[must_use]
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
        }
    }

    /// Распознаёт готовый WAV-файл.
    ///
    /// `language`: `Some("ru")` — принудительный язык, `None` — автоопределение.
    ///
    /// # Errors
    /// Возвращает [`SttError`] при сетевой ошибке, неуспешном статусе
    /// или ответе без транскрипта.
    pub async fn transcribe_wav(
        &self,
        wav: Vec<u8>,
        language: Option<&str>,
    ) -> Result<Transcript, SttError> {
        let response = self
            .client
            .post(build_url(language))
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", "audio/wav")
            .body(wav)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SttError::Api {
                status: status.as_u16(),
                body,
            });
        }
        parse_transcript(&response.text().await?, language)
    }
}

/// Собирает URL запроса: модель, пунктуация и режим определения языка.
fn build_url(language: Option<&str>) -> reqwest::Url {
    let language_param = match language {
        Some(code) => ("language", code),
        None => ("detect_language", "true"),
    };
    let params = [("model", MODEL), ("smart_format", "true"), language_param];
    #[allow(clippy::expect_used)] // константный URL, ошибка возможна только при опечатке
    reqwest::Url::parse_with_params(API_URL, params).expect("API_URL договорно валиден")
}

/// Достаёт текст и язык из JSON-ответа API.
fn parse_transcript(body: &str, forced_language: Option<&str>) -> Result<Transcript, SttError> {
    let parsed: ApiResponse = serde_json::from_str(body).map_err(|_| SttError::EmptyResponse)?;
    let channel = parsed
        .results
        .channels
        .into_iter()
        .next()
        .ok_or(SttError::EmptyResponse)?;
    let detected = channel.detected_language;
    let alternative = channel
        .alternatives
        .into_iter()
        .next()
        .ok_or(SttError::EmptyResponse)?;

    let language = forced_language
        .map(String::from)
        .or(detected)
        .unwrap_or_else(|| String::from("en"));
    Ok(Transcript {
        text: alternative.transcript.trim().to_owned(),
        language,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_with_forced_language() {
        let url = build_url(Some("ru")).to_string();
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("smart_format=true"));
        assert!(url.contains("language=ru"));
        assert!(!url.contains("detect_language"));
    }

    #[test]
    fn url_with_language_detection() {
        let url = build_url(None).to_string();
        assert!(url.contains("detect_language=true"));
    }

    fn api_response(transcript: &str, detected: Option<&str>) -> String {
        let detected = detected.map_or(String::from("null"), |code| format!("\"{code}\""));
        format!(
            r#"{{"results":{{"channels":[{{"detected_language":{detected},"alternatives":[{{"transcript":"{transcript}"}}]}}]}}}}"#
        )
    }

    #[test]
    fn parses_transcript_with_detected_language() {
        let body = api_response("Hello there.", Some("en"));
        let transcript = parse_transcript(&body, None).unwrap();
        assert_eq!(
            transcript,
            Transcript {
                text: "Hello there.".into(),
                language: "en".into()
            }
        );
    }

    #[test]
    fn forced_language_wins_over_detected() {
        let body = api_response("Привет.", Some("en"));
        assert_eq!(parse_transcript(&body, Some("ru")).unwrap().language, "ru");
    }

    #[test]
    fn rejects_response_without_channels() {
        assert!(matches!(
            parse_transcript(r#"{"results":{"channels":[]}}"#, None),
            Err(SttError::EmptyResponse)
        ));
    }
}
