//! Минимальный разбор WAV (RIFF, PCM 16-bit) для режима реплея:
//! прогон записанного файла через live-пайплайн вместо микрофона.

use thiserror::Error;

/// Ошибки разбора WAV-файла.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WavError {
    /// Файл не похож на RIFF/WAVE.
    #[error("это не WAV-файл (нет заголовка RIFF/WAVE)")]
    NotRiff,
    /// Файл оборван или чанки не сходятся по размеру.
    #[error("WAV-файл повреждён или оборван")]
    Truncated,
    /// Поддерживается только несжатый PCM 16-bit.
    #[error("поддерживается только PCM 16-bit, здесь формат {format}, {bits} бит")]
    UnsupportedFormat {
        /// Код формата из fmt-чанка (1 = PCM).
        format: u16,
        /// Разрядность сэмпла.
        bits: u16,
    },
}

/// Разобранный WAV: параметры и сэмплы в f32.
pub struct WavAudio {
    /// Частота дискретизации.
    pub sample_rate: u32,
    /// Число каналов (interleaved).
    pub channels: u16,
    /// Сэмплы в диапазоне [-1.0; 1.0].
    pub samples: Vec<f32>,
}

/// Разбирает WAV-файл из байтов.
///
/// # Errors
/// Возвращает [`WavError`], если файл не RIFF, оборван или не PCM 16-bit.
pub fn parse_wav(bytes: &[u8]) -> Result<WavAudio, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotRiff);
    }

    let mut format = None;
    let mut data = None;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = read_u32(bytes, offset + 4)? as usize;
        let body_start = offset + 8;
        let body_end = body_start.checked_add(size).ok_or(WavError::Truncated)?;
        if body_end > bytes.len() {
            return Err(WavError::Truncated);
        }
        match id {
            b"fmt " => format = Some(parse_format(&bytes[body_start..body_end])?),
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        // Чанки выравниваются по чётной границе.
        offset = body_end + (size % 2);
    }

    let (sample_rate, channels) = format.ok_or(WavError::Truncated)?;
    let pcm = data.ok_or(WavError::Truncated)?;
    let samples = pcm
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / f32::from(i16::MAX))
        .collect();
    Ok(WavAudio {
        sample_rate,
        channels,
        samples,
    })
}

/// Возвращает (частота, каналы) из fmt-чанка, проверяя формат PCM 16-bit.
fn parse_format(body: &[u8]) -> Result<(u32, u16), WavError> {
    if body.len() < 16 {
        return Err(WavError::Truncated);
    }
    let format = read_u16(body, 0)?;
    let channels = read_u16(body, 2)?;
    let sample_rate = read_u32(body, 4)?;
    let bits = read_u16(body, 14)?;
    if format != 1 || bits != 16 {
        return Err(WavError::UnsupportedFormat { format, bits });
    }
    Ok((sample_rate, channels))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, WavError> {
    let slice = bytes.get(offset..offset + 2).ok_or(WavError::Truncated)?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, WavError> {
    let slice = bytes.get(offset..offset + 4).ok_or(WavError::Truncated)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает валидный WAV с указанными PCM-сэмплами.
    fn make_wav(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&u32::try_from(36 + pcm.len()).unwrap().to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(pcm.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&pcm);
        out
    }

    #[test]
    fn parses_valid_pcm16_wav() {
        let wav = make_wav(22_050, 1, &[0, i16::MAX, -i16::MAX]);
        let audio = parse_wav(&wav).unwrap();
        assert_eq!(audio.sample_rate, 22_050);
        assert_eq!(audio.channels, 1);
        assert_eq!(audio.samples.len(), 3);
        assert!((audio.samples[1] - 1.0).abs() < 1e-6);
        assert!((audio.samples[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_non_riff() {
        assert!(matches!(parse_wav(b"OGGS----WAVE"), Err(WavError::NotRiff)));
    }

    #[test]
    fn rejects_truncated_file() {
        let mut wav = make_wav(16_000, 1, &[1, 2, 3]);
        wav.truncate(wav.len() - 2);
        assert!(matches!(parse_wav(&wav), Err(WavError::Truncated)));
    }

    #[test]
    fn rejects_float_wav() {
        let mut wav = make_wav(16_000, 1, &[0]);
        wav[20] = 3; // формат 3 = IEEE float
        assert!(matches!(
            parse_wav(&wav),
            Err(WavError::UnsupportedFormat { format: 3, .. })
        ));
    }
}
