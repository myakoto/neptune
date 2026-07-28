//! Стриминговый клиент Deepgram (WebSocket, endpoint `/v1/listen`).
//!
//! Соединение держится на всю сессию: модель сама сегментирует речь
//! (endpointing), сохраняет контекст между паузами и присваивает
//! спикеров словам (`diarize`). Аудио — linear16, 16 kHz, mono.

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const API_URL: &str = "wss://api.deepgram.com/v1/listen";
const MODEL: &str = "nova-3";

/// Ошибки стриминговой сессии.
#[derive(Debug, Error)]
pub enum LiveError {
    /// Ошибка WebSocket-соединения.
    #[error("WebSocket Deepgram: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    /// Не удалось собрать запрос на подключение.
    #[error("не удалось подготовить запрос к Deepgram: {0}")]
    Request(String),
}

/// Параметры live-сессии.
pub struct LiveOptions {
    /// Код языка ("en", "ru", "multi").
    pub language: String,
    /// Присваивать ли спикеров словам.
    pub diarize: bool,
    /// Частота отправляемого аудио, Hz.
    pub sample_rate: u32,
}

/// Событие распознавания, готовое к показу.
#[derive(Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// Промежуточная гипотеза (будет уточнена).
    Interim(String),
    /// Финализированный фрагмент.
    Final {
        /// Распознанный текст.
        text: String,
        /// Номер спикера, если включена диаризация.
        speaker: Option<u32>,
    },
    /// Служебное сообщение, для отображения не нужно.
    Ignored,
}

#[derive(Deserialize)]
struct LiveMessage {
    #[serde(rename = "type")]
    kind: String,
    is_final: Option<bool>,
    channel: Option<Channel>,
}

#[derive(Deserialize)]
struct Channel {
    alternatives: Vec<Alternative>,
}

#[derive(Deserialize)]
struct Alternative {
    transcript: String,
    #[serde(default)]
    words: Vec<Word>,
}

#[derive(Deserialize)]
struct Word {
    speaker: Option<u32>,
}

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WsSource = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

/// Подключается к live-endpoint'у с указанным ключом и параметрами.
///
/// Возвращает раздельные половины соединения: отправку аудио и чтение
/// событий можно гонять в независимых задачах.
///
/// # Errors
/// Возвращает [`LiveError`] при сбое подключения или сборки запроса.
pub async fn connect(
    api_key: &str,
    options: &LiveOptions,
) -> Result<(LiveSender, LiveReceiver), LiveError> {
    let mut request = build_url(options)
        .as_str()
        .into_client_request()
        .map_err(|e| LiveError::Request(e.to_string()))?;
    let auth = format!("Token {api_key}")
        .parse()
        .map_err(|_| LiveError::Request("ключ содержит недопустимые символы".into()))?;
    request.headers_mut().insert(AUTHORIZATION, auth);

    let (ws, _) = connect_async(request).await?;
    let (sink, source) = ws.split();
    Ok((LiveSender { sink }, LiveReceiver { source }))
}

/// Отправляющая половина сессии: аудио и служебные сообщения.
pub struct LiveSender {
    sink: WsSink,
}

impl LiveSender {
    /// Отправляет чанк 16-битного PCM-аудио.
    ///
    /// # Errors
    /// Возвращает [`LiveError`] при сбое отправки.
    pub async fn send_audio(&mut self, pcm: &[i16]) -> Result<(), LiveError> {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        Ok(self.sink.send(Message::binary(bytes)).await?)
    }

    /// Держит соединение живым во время тишины.
    ///
    /// # Errors
    /// Возвращает [`LiveError`] при сбое отправки.
    pub async fn keep_alive(&mut self) -> Result<(), LiveError> {
        Ok(self
            .sink
            .send(Message::text(r#"{"type":"KeepAlive"}"#))
            .await?)
    }

    /// Просит сервер финализировать остаток и закрыть поток.
    ///
    /// # Errors
    /// Возвращает [`LiveError`] при сбое отправки.
    pub async fn close_stream(&mut self) -> Result<(), LiveError> {
        Ok(self
            .sink
            .send(Message::text(r#"{"type":"CloseStream"}"#))
            .await?)
    }
}

/// Читающая половина сессии: события распознавания.
pub struct LiveReceiver {
    source: WsSource,
}

impl LiveReceiver {
    /// Ждёт следующее событие; `None` — сервер закрыл соединение.
    pub async fn next_event(&mut self) -> Option<LiveEvent> {
        loop {
            match self.source.next().await? {
                Ok(Message::Text(payload)) => return Some(parse_event(payload.as_str())),
                Ok(Message::Close(_)) => return None,
                Ok(_) => {}
                Err(error) => {
                    eprintln!("ошибка чтения из Deepgram: {error}");
                    return None;
                }
            }
        }
    }
}

/// Собирает URL live-endpoint'а под наш формат аудио.
fn build_url(options: &LiveOptions) -> reqwest::Url {
    let sample_rate = options.sample_rate.to_string();
    let mut params = vec![
        ("model", MODEL),
        ("smart_format", "true"),
        ("interim_results", "true"),
        ("encoding", "linear16"),
        ("channels", "1"),
        ("sample_rate", sample_rate.as_str()),
        ("language", options.language.as_str()),
    ];
    if options.diarize {
        params.push(("diarize", "true"));
    }
    #[allow(clippy::expect_used)] // константный URL, ошибка возможна только при опечатке
    reqwest::Url::parse_with_params(API_URL, params).expect("API_URL договорно валиден")
}

/// Разбирает текстовое сообщение сервера в [`LiveEvent`].
fn parse_event(payload: &str) -> LiveEvent {
    let Ok(message) = serde_json::from_str::<LiveMessage>(payload) else {
        return LiveEvent::Ignored;
    };
    if message.kind != "Results" {
        return LiveEvent::Ignored;
    }
    let Some(alternative) = message
        .channel
        .and_then(|c| c.alternatives.into_iter().next())
    else {
        return LiveEvent::Ignored;
    };
    let text = alternative.transcript.trim().to_owned();
    if text.is_empty() {
        return LiveEvent::Ignored;
    }
    if message.is_final == Some(true) {
        let speaker = alternative.words.first().and_then(|w| w.speaker);
        LiveEvent::Final { text, speaker }
    } else {
        LiveEvent::Interim(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> LiveOptions {
        LiveOptions {
            language: "en".into(),
            diarize: true,
            sample_rate: 16_000,
        }
    }

    #[test]
    fn url_contains_streaming_params() {
        let url = build_url(&options()).to_string();
        for expected in [
            "model=nova-3",
            "smart_format=true",
            "interim_results=true",
            "encoding=linear16",
            "sample_rate=16000",
            "language=en",
            "diarize=true",
        ] {
            assert!(url.contains(expected), "нет {expected} в {url}");
        }
    }

    #[test]
    fn url_omits_diarize_when_disabled() {
        let url = build_url(&LiveOptions {
            diarize: false,
            ..options()
        })
        .to_string();
        assert!(!url.contains("diarize"));
    }

    #[test]
    fn parse_interim_result() {
        let payload = r#"{"type":"Results","is_final":false,
            "channel":{"alternatives":[{"transcript":"hello wor"}]}}"#;
        assert_eq!(parse_event(payload), LiveEvent::Interim("hello wor".into()));
    }

    #[test]
    fn parse_final_result_with_speaker() {
        let payload = r#"{"type":"Results","is_final":true,
            "channel":{"alternatives":[{"transcript":"Hello world.",
            "words":[{"word":"hello","speaker":1},{"word":"world","speaker":1}]}]}}"#;
        assert_eq!(
            parse_event(payload),
            LiveEvent::Final {
                text: "Hello world.".into(),
                speaker: Some(1)
            }
        );
    }

    #[test]
    fn parse_skips_metadata_and_empty() {
        assert_eq!(parse_event(r#"{"type":"Metadata"}"#), LiveEvent::Ignored);
        let empty = r#"{"type":"Results","is_final":true,
            "channel":{"alternatives":[{"transcript":"  "}]}}"#;
        assert_eq!(parse_event(empty), LiveEvent::Ignored);
        assert_eq!(parse_event("not json"), LiveEvent::Ignored);
    }
}
