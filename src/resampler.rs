use crate::{Result, invalid_input};

pub struct AudioResampler {
    step: f64,
    phase: f64,
    carry_left: f32,
    carry_right: f32,
    has_carry: bool,
    current_gain: f32,
    target_gain: f32,
}

impl AudioResampler {
    pub fn new(source_rate: u32, target_rate: u32, gain: f32) -> Result<Self> {
        if source_rate == 0 || target_rate == 0 {
            return Err(invalid_input("sample rates must be nonzero"));
        }
        let gain = gain.clamp(0.5, 2.5);
        Ok(Self {
            step: f64::from(source_rate) / f64::from(target_rate),
            phase: 0.0,
            carry_left: 0.0,
            carry_right: 0.0,
            has_carry: false,
            current_gain: gain,
            target_gain: gain,
        })
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.target_gain = gain.clamp(0.5, 2.5);
    }

    pub fn resample(
        &mut self,
        interleaved: &[f32],
        out_left: &mut [i16],
        out_right: &mut [i16],
    ) -> usize {
        let input_frames = interleaved.len() / 2;
        if input_frames == 0 {
            return 0;
        }
        let max_outputs = out_left.len().min(out_right.len());
        let mut output_frames = 0;
        let mut position = self.phase;

        if self.has_carry {
            while position < 0.0 && output_frames < max_outputs {
                let fraction = (position + 1.0) as f32;
                let left = self.carry_left * (1.0 - fraction) + interleaved[0] * fraction;
                let right = self.carry_right * (1.0 - fraction) + interleaved[1] * fraction;
                self.ramp_gain();
                out_left[output_frames] = float_to_pcm16(left, self.current_gain);
                out_right[output_frames] = float_to_pcm16(right, self.current_gain);
                output_frames += 1;
                position += self.step;
            }
        } else if position < 0.0 {
            position = 0.0;
        }

        while position < (input_frames - 1) as f64 && output_frames < max_outputs {
            let current = position as usize;
            let fraction = (position - current as f64) as f32;
            let left = interleaved[current * 2] * (1.0 - fraction)
                + interleaved[(current + 1) * 2] * fraction;
            let right = interleaved[current * 2 + 1] * (1.0 - fraction)
                + interleaved[(current + 1) * 2 + 1] * fraction;
            self.ramp_gain();
            out_left[output_frames] = float_to_pcm16(left, self.current_gain);
            out_right[output_frames] = float_to_pcm16(right, self.current_gain);
            output_frames += 1;
            position += self.step;
        }

        self.carry_left = interleaved[(input_frames - 1) * 2];
        self.carry_right = interleaved[(input_frames - 1) * 2 + 1];
        self.has_carry = true;
        self.phase = position - input_frames as f64;
        output_frames
    }

    fn ramp_gain(&mut self) {
        if (self.current_gain - self.target_gain).abs() > 0.0001 {
            self.current_gain += (self.target_gain - self.current_gain) * 0.002;
        } else {
            self.current_gain = self.target_gain;
        }
    }
}

pub fn float_to_pcm16(value: f32, gain: f32) -> i16 {
    const SAFE_CEILING: f32 = 0.85;
    const LINEAR_KNEE: f32 = 0.60;
    const HEADROOM: f32 = SAFE_CEILING - LINEAR_KNEE;

    let boosted = value * gain;
    let limited = if boosted > LINEAR_KNEE {
        LINEAR_KNEE + HEADROOM * ((boosted - LINEAR_KNEE) / HEADROOM).tanh()
    } else if boosted < -LINEAR_KNEE {
        -LINEAR_KNEE - HEADROOM * ((-boosted - LINEAR_KNEE) / HEADROOM).tanh()
    } else {
        boosted
    };
    let clamped = limited.clamp(-SAFE_CEILING, SAFE_CEILING);
    let sample = if clamped < 0.0 {
        clamped * 32768.0
    } else {
        clamped * 32767.0
    };
    sample.round_ties_even() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_48khz_to_32khz_at_correct_ratio() {
        let mut resampler = AudioResampler::new(48_000, 32_000, 1.0).unwrap();
        let input = vec![0.0; 960];
        let mut left = [0; 512];
        let mut right = [0; 512];
        assert_eq!(320, resampler.resample(&input, &mut left, &mut right));
    }

    #[test]
    fn split_chunks_preserve_phase_and_sample_count() {
        let mut full_input = vec![0.0; 960 * 2];
        for index in 0..960 {
            let sample = (std::f32::consts::TAU * 440.0 * index as f32 / 48_000.0).sin() * 0.7;
            full_input[index * 2] = sample;
            full_input[index * 2 + 1] = sample;
        }
        let mut single = AudioResampler::new(48_000, 32_000, 1.0).unwrap();
        let mut split = AudioResampler::new(48_000, 32_000, 1.0).unwrap();
        let mut expected_l = [0; 700];
        let mut expected_r = [0; 700];
        let mut actual_l = [0; 700];
        let mut actual_r = [0; 700];
        let expected = single.resample(&full_input, &mut expected_l, &mut expected_r);
        let first = split.resample(&full_input[..960], &mut actual_l, &mut actual_r);
        let second = split.resample(
            &full_input[960..],
            &mut actual_l[first..],
            &mut actual_r[first..],
        );
        assert_eq!(expected, first + second);
        for index in 0..expected {
            assert!((expected_l[index] - actual_l[index]).abs() <= 1);
            assert!((expected_r[index] - actual_r[index]).abs() <= 1);
        }
    }

    #[test]
    fn limiter_preserves_headroom() {
        assert!(float_to_pcm16(10.0, 1.0) <= 27_852);
        assert!(float_to_pcm16(-10.0, 1.0) >= -27_853);
        assert!(float_to_pcm16(1.5, 1.0) >= float_to_pcm16(1.2, 1.0));
        assert_eq!(16_384, float_to_pcm16(0.5, 1.0));
    }

    #[test]
    fn gain_is_clamped() {
        let mut resampler = AudioResampler::new(48_000, 32_000, 3.0).unwrap();
        assert_eq!(2.5, resampler.target_gain);
        resampler.set_gain(0.1);
        assert_eq!(0.5, resampler.target_gain);
    }
}
