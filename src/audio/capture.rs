//! Захват звука через cpal (на Windows — WASAPI): микрофон или loopback.
//!
//! Loopback — штатная возможность WASAPI: входной поток открывается
//! на устройстве *вывода* и получает копию всего, что в нём играет
//! (голос собеседника в созвоне). Поток cpal не `Send`, поэтому живёт
//! на выделенном std-треде; чанки уходят в tokio-канал.
//! Дроп [`AudioCapture`] останавливает захват.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, SupportedStreamConfig};
use thiserror::Error;
use tokio::sync::mpsc;

/// Ошибки захвата звука.
#[derive(Debug, Error)]
pub enum AudioError {
    /// В системе нет подходящего устройства по умолчанию.
    #[error("не найдено устройство по умолчанию: {0}")]
    NoDevice(&'static str),
    /// Ошибка cpal при опросе или запуске устройства.
    #[error("аудиоустройство: {0}")]
    Device(String),
    /// Формат сэмплов устройства не поддержан.
    #[error("неподдерживаемый формат сэмплов: {0}")]
    UnsupportedFormat(String),
}

/// Что захватываем.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    /// Устройство ввода по умолчанию (моя речь).
    Mic,
    /// Loopback устройства вывода по умолчанию (речь собеседников).
    Loopback,
}

/// Запущенный захват звука.
pub struct AudioCapture {
    /// Частота дискретизации устройства.
    pub sample_rate: u32,
    /// Число каналов устройства.
    pub channels: u16,
    /// Чанки interleaved-сэмплов f32.
    pub chunks: mpsc::UnboundedReceiver<Vec<f32>>,
    /// Пока жив этот отправитель, жив и тред захвата.
    _stop: std::sync::mpsc::Sender<()>,
}

impl AudioCapture {
    /// Запускает захват с выбранного источника.
    ///
    /// # Errors
    /// Возвращает [`AudioError`], если устройства нет, его формат
    /// не поддержан или поток не удалось запустить.
    pub fn start(source: CaptureSource) -> Result<Self, AudioError> {
        let (startup_tx, startup_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();

        std::thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || capture_thread(source, &startup_tx, &stop_rx, &chunk_tx))
            .map_err(|e| AudioError::Device(e.to_string()))?;

        let (sample_rate, channels) = startup_rx
            .recv()
            .map_err(|_| AudioError::Device("тред захвата завершился при старте".into()))??;
        Ok(Self {
            sample_rate,
            channels,
            chunks: chunk_rx,
            _stop: stop_tx,
        })
    }
}

type StartupResult = Result<(u32, u16), AudioError>;

/// Тело треда: строит cpal-поток, сообщает формат и ждёт сигнала остановки.
fn capture_thread(
    source: CaptureSource,
    startup: &std::sync::mpsc::Sender<StartupResult>,
    stop: &std::sync::mpsc::Receiver<()>,
    chunks: &mpsc::UnboundedSender<Vec<f32>>,
) {
    let stream = match build_stream(source, chunks.clone()) {
        Ok((stream, rate, channels)) => {
            let _ = startup.send(Ok((rate, channels)));
            stream
        }
        Err(error) => {
            let _ = startup.send(Err(error));
            return;
        }
    };
    if let Err(error) = stream.play() {
        eprintln!("не удалось запустить аудиопоток: {error}");
        return;
    }
    // Блокируемся, пока AudioCapture не уронит отправитель.
    let _ = stop.recv();
}

/// Возвращает устройство и конфиг под выбранный источник.
fn device_and_config(source: CaptureSource) -> Result<(Device, SupportedStreamConfig), AudioError> {
    let host = cpal::default_host();
    match source {
        CaptureSource::Mic => {
            let device = host
                .default_input_device()
                .ok_or(AudioError::NoDevice("нет микрофона"))?;
            let config = device
                .default_input_config()
                .map_err(|e| AudioError::Device(e.to_string()))?;
            Ok((device, config))
        }
        CaptureSource::Loopback => {
            let device = host
                .default_output_device()
                .ok_or(AudioError::NoDevice("нет устройства вывода"))?;
            // Для loopback входной поток открывается с конфигом вывода.
            let config = device
                .default_output_config()
                .map_err(|e| AudioError::Device(e.to_string()))?;
            Ok((device, config))
        }
    }
}

fn build_stream(
    source: CaptureSource,
    chunks: mpsc::UnboundedSender<Vec<f32>>,
) -> Result<(cpal::Stream, u32, u16), AudioError> {
    let (device, config) = device_and_config(source)?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();

    let on_error = |error| eprintln!("ошибка аудиопотока: {error}");
    let stream = match config.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &_| {
                let _ = chunks.send(data.to_vec());
            },
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _: &_| {
                let _ = chunks.send(
                    data.iter()
                        .map(|&s| f32::from(s) / f32::from(i16::MAX))
                        .collect(),
                );
            },
            on_error,
            None,
        ),
        other => return Err(AudioError::UnsupportedFormat(other.to_string())),
    }
    .map_err(|e| AudioError::Device(e.to_string()))?;

    Ok((stream, sample_rate, channels))
}
