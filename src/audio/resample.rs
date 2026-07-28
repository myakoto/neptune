//! Чистые преобразования звука: даунмикс в моно и линейный ресемплинг.
//!
//! Для речи линейной интерполяции достаточно; полосовые фильтры не нужны,
//! потому что дальше звук уходит в STT, а не в уши.

/// Смешивает интерлив-каналы в моно усреднением.
#[must_use]
pub fn downmix(input: &[f32], channels: u16) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    if channels == 1 {
        return input.to_vec();
    }
    #[allow(clippy::cast_precision_loss)] // каналов единицы, точности f32 хватает
    input
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Потоковый линейный ресемплер: помнит хвост предыдущего чанка,
/// чтобы на границах не было щелчков и потерянных сэмплов.
pub struct Resampler {
    step: f64,
    /// Дробная позиция чтения относительно начала виртуального входа
    /// (последний сэмпл прошлого чанка + текущий чанк).
    pos: f64,
    carry: Option<f32>,
}

impl Resampler {
    /// Создаёт ресемплер из частоты `from` в частоту `to`.
    #[must_use]
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from) / f64::from(to),
            pos: 0.0,
            carry: None,
        }
    }

    /// Обрабатывает очередной моно-чанк, возвращая 16-битные сэмплы.
    pub fn process(&mut self, mono: &[f32]) -> Vec<i16> {
        if mono.is_empty() {
            return Vec::new();
        }
        let input: Vec<f32> = match self.carry.take() {
            Some(last) => std::iter::once(last).chain(mono.iter().copied()).collect(),
            None => mono.to_vec(),
        };

        let mut output = Vec::with_capacity(estimated_len(input.len(), self.step));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // pos неотрицателен и ограничен длиной чанка
        while self.pos + 1.0 < approx_len(&input) {
            let index = self.pos as usize;
            let frac = self.pos - self.pos.floor();
            #[allow(clippy::cast_possible_truncation)]
            let sample =
                f64::from(input[index]) * (1.0 - frac) + f64::from(input[index + 1]) * frac;
            output.push(quantize(sample as f32));
            self.pos += self.step;
        }

        // Следующий чанк продолжится от последнего сэмпла текущего.
        #[allow(clippy::cast_precision_loss)]
        {
            self.pos -= (input.len() - 1) as f64;
        }
        self.carry = input.last().copied();
        output
    }
}

#[allow(clippy::cast_precision_loss)]
fn approx_len(input: &[f32]) -> f64 {
    input.len() as f64
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn estimated_len(input_len: usize, step: f64) -> usize {
    (input_len as f64 / step) as usize + 2
}

/// Переводит f32-сэмпл [-1.0; 1.0] в i16 с ограничением диапазона.
#[allow(clippy::cast_possible_truncation)] // clamp гарантирует диапазон i16
fn quantize(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_averages_channels() {
        let stereo = [0.5, -0.5, 1.0, 0.0];
        assert_eq!(downmix(&stereo, 2), vec![0.0, 0.5]);
    }

    #[test]
    fn downmix_mono_is_identity() {
        let mono = [0.1, 0.2];
        assert_eq!(downmix(&mono, 1), mono.to_vec());
    }

    #[test]
    fn identity_rate_keeps_sample_count() {
        let mut resampler = Resampler::new(16_000, 16_000);
        let total: usize = (0..10).map(|_| resampler.process(&[0.25; 480]).len()).sum();
        // Потоковая обрезка может задержать 1 сэмпл в carry.
        assert!((4799..=4800).contains(&total), "получили {total}");
    }

    #[test]
    fn downsampling_48k_to_16k_thins_by_three() {
        let mut resampler = Resampler::new(48_000, 16_000);
        let chunk = vec![0.5; 4800];
        let total: usize = (0..10).map(|_| resampler.process(&chunk).len()).sum();
        let expected = 4800 * 10 / 3;
        assert!(
            total.abs_diff(expected) <= 1,
            "получили {total}, ждали ~{expected}"
        );
    }

    #[test]
    fn quantize_clamps_out_of_range() {
        assert_eq!(quantize(2.0), i16::MAX);
        assert_eq!(quantize(-2.0), -i16::MAX);
        assert_eq!(quantize(0.0), 0);
    }
}
