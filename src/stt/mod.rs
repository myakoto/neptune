//! Распознавание речи (speech-to-text). Бэкенд — Deepgram Nova-3:
//! REST для готовых файлов, WebSocket для живого стрима.

mod deepgram;
pub mod live;

pub use deepgram::DeepgramClient;
