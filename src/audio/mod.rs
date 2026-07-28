//! Захват и подготовка звука: микрофон (cpal/WASAPI) и ресемплинг
//! в формат, который ждёт Deepgram (16 kHz, mono, linear16).

mod capture;
mod resample;
mod wav;

pub use capture::{AudioCapture, CaptureSource};
pub use resample::{Resampler, downmix};
pub use wav::parse_wav;

/// Частота, в которую приводится звук перед отправкой в STT.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
