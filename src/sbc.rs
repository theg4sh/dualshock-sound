use crate::protocol::{SAMPLES_PER_FRAME, SBC_FRAME_LENGTH};
use crate::{Result, invalid_input};

const CHANNELS: usize = 2;
const SUBBANDS: usize = 8;
const BLOCKS: usize = 16;
const BITPOOL: i32 = 48;
const HISTORY_LENGTH: usize = 80;

const PROTOTYPE_8: [f64; HISTORY_LENGTH] = [
    0.00000000E+00,
    1.56575398E-04,
    3.43256425E-04,
    5.54620202E-04,
    8.23919506E-04,
    1.13992507E-03,
    1.47640169E-03,
    1.78371725E-03,
    2.01182542E-03,
    2.10371989E-03,
    1.99454554E-03,
    1.61656283E-03,
    9.02154502E-04,
    -1.78805361E-04,
    -1.64973098E-03,
    -3.49717454E-03,
    5.65949473E-03,
    8.02941163E-03,
    1.04584443E-02,
    1.27472335E-02,
    1.46525263E-02,
    1.59045603E-02,
    1.62208471E-02,
    1.53184106E-02,
    1.29371806E-02,
    8.85757540E-03,
    2.92408442E-03,
    -4.91578024E-03,
    -1.46404076E-02,
    -2.61098752E-02,
    -3.90751381E-02,
    -5.31873032E-02,
    6.79989431E-02,
    8.29847578E-02,
    9.75753918E-02,
    1.11196689E-01,
    1.23264548E-01,
    1.33264415E-01,
    1.40753505E-01,
    1.45389847E-01,
    1.46955068E-01,
    1.45389847E-01,
    1.40753505E-01,
    1.33264415E-01,
    1.23264548E-01,
    1.11196689E-01,
    9.75753918E-02,
    8.29847578E-02,
    -6.79989431E-02,
    -5.31873032E-02,
    -3.90751381E-02,
    -2.61098752E-02,
    -1.46404076E-02,
    -4.91578024E-03,
    2.92408442E-03,
    8.85757540E-03,
    1.29371806E-02,
    1.53184106E-02,
    1.62208471E-02,
    1.59045603E-02,
    1.46525263E-02,
    1.27472335E-02,
    1.04584443E-02,
    8.02941163E-03,
    -5.65949473E-03,
    -3.49717454E-03,
    -1.64973098E-03,
    -1.78805361E-04,
    9.02154502E-04,
    1.61656283E-03,
    1.99454554E-03,
    2.10371989E-03,
    2.01182542E-03,
    1.78371725E-03,
    1.47640169E-03,
    1.13992507E-03,
    8.23919506E-04,
    5.54620202E-04,
    3.43256425E-04,
    1.56575398E-04,
];

pub struct DualShock4SbcEncoder {
    configuration: u8,
    analysis_matrix: [[f64; HISTORY_LENGTH]; SUBBANDS],
    history: [[f64; HISTORY_LENGTH]; CHANNELS],
    subband_samples: [[f64; SAMPLES_PER_FRAME]; CHANNELS],
    scale_factors: [[i32; SUBBANDS]; CHANNELS],
    allocated_bits: [[i32; SUBBANDS]; CHANNELS],
    joined: [bool; SUBBANDS],
}

impl DualShock4SbcEncoder {
    pub fn new(sample_rate: u32) -> Result<Self> {
        let configuration = match sample_rate {
            16_000 => 0x3f,
            32_000 => 0x7f,
            _ => return Err(invalid_input("SBC supports only 16 kHz and 32 kHz")),
        };
        let mut analysis_matrix = [[0.0; HISTORY_LENGTH]; SUBBANDS];
        for (band, row) in analysis_matrix.iter_mut().enumerate() {
            for (tap, value) in row.iter_mut().enumerate() {
                let sign = if (tap / 16) & 1 == 0 { 1.0 } else { -1.0 };
                let angle = (band as f64 + 0.5)
                    * (tap as f64 - SUBBANDS as f64 / 2.0)
                    * std::f64::consts::PI
                    / SUBBANDS as f64;
                *value = PROTOTYPE_8[tap] * sign * angle.cos();
            }
        }
        Ok(Self {
            configuration,
            analysis_matrix,
            history: [[0.0; HISTORY_LENGTH]; CHANNELS],
            subband_samples: [[0.0; SAMPLES_PER_FRAME]; CHANNELS],
            scale_factors: [[0; SUBBANDS]; CHANNELS],
            allocated_bits: [[0; SUBBANDS]; CHANNELS],
            joined: [false; SUBBANDS],
        })
    }

    pub fn reset(&mut self) {
        self.history = [[0.0; HISTORY_LENGTH]; CHANNELS];
        self.subband_samples = [[0.0; SAMPLES_PER_FRAME]; CHANNELS];
    }

    pub fn encode(&mut self, left: &[i16], right: &[i16]) -> Result<[u8; SBC_FRAME_LENGTH]> {
        if left.len() < SAMPLES_PER_FRAME || right.len() < SAMPLES_PER_FRAME {
            return Err(invalid_input("an SBC frame needs 128 samples per channel"));
        }
        self.analyze_channel(0, left);
        self.analyze_channel(1, right);
        self.select_joint_stereo_bands();
        self.calculate_scale_factors();
        self.allocate_snr_bits();
        self.write_frame()
    }

    pub fn silence() -> [u8; SBC_FRAME_LENGTH] {
        let zeros = [0i16; SAMPLES_PER_FRAME];
        Self::new(32_000).unwrap().encode(&zeros, &zeros).unwrap()
    }

    fn analyze_channel(&mut self, channel: usize, pcm: &[i16]) {
        for block in 0..BLOCKS {
            let input_offset = block * SUBBANDS;
            let output_offset = block * SUBBANDS;
            self.history[channel].copy_within(0..HISTORY_LENGTH - SUBBANDS, SUBBANDS);
            for sample in 0..SUBBANDS {
                self.history[channel][SUBBANDS - 1 - sample] =
                    f64::from(pcm[input_offset + sample]);
            }
            for band in 0..SUBBANDS {
                let mut value = 0.0;
                for tap in 0..HISTORY_LENGTH {
                    value += self.analysis_matrix[band][tap] * self.history[channel][tap];
                }
                self.subband_samples[channel][output_offset + band] = value;
            }
        }
    }

    fn select_joint_stereo_bands(&mut self) {
        for band in 0..SUBBANDS - 1 {
            let mut maximum_left = 0.0f64;
            let mut maximum_right = 0.0f64;
            let mut maximum_sum = 0.0f64;
            let mut maximum_difference = 0.0f64;
            for block in 0..BLOCKS {
                let index = block * SUBBANDS + band;
                let left = self.subband_samples[0][index];
                let right = self.subband_samples[1][index];
                maximum_left = maximum_left.max(left.abs());
                maximum_right = maximum_right.max(right.abs());
                maximum_sum = maximum_sum.max(((left + right) * 0.5).abs());
                maximum_difference = maximum_difference.max(((left - right) * 0.5).abs());
            }
            let use_joint = scale_factor(maximum_sum) + scale_factor(maximum_difference)
                < scale_factor(maximum_left) + scale_factor(maximum_right);
            self.joined[band] = use_joint;
            if !use_joint {
                continue;
            }
            for block in 0..BLOCKS {
                let index = block * SUBBANDS + band;
                let left = self.subband_samples[0][index];
                let right = self.subband_samples[1][index];
                self.subband_samples[0][index] = (left + right) * 0.5;
                self.subband_samples[1][index] = (left - right) * 0.5;
            }
        }
        self.joined[SUBBANDS - 1] = false;
    }

    fn calculate_scale_factors(&mut self) {
        for channel in 0..CHANNELS {
            for band in 0..SUBBANDS {
                let mut maximum = 0.0f64;
                for block in 0..BLOCKS {
                    maximum =
                        maximum.max(self.subband_samples[channel][block * SUBBANDS + band].abs());
                }
                self.scale_factors[channel][band] = scale_factor(maximum);
            }
        }
    }

    fn allocate_snr_bits(&mut self) {
        let mut maximum_need = 0;
        for channel in 0..CHANNELS {
            for band in 0..SUBBANDS {
                maximum_need = maximum_need.max(self.scale_factors[channel][band]);
                self.allocated_bits[channel][band] = 0;
            }
        }

        let mut bit_count = 0;
        let mut slice_count = 0;
        let mut bit_slice = maximum_need + 1;
        loop {
            bit_slice -= 1;
            bit_count += slice_count;
            slice_count = 0;
            for channel in 0..CHANNELS {
                for band in 0..SUBBANDS {
                    let need = self.scale_factors[channel][band];
                    if need > bit_slice + 1 && need < bit_slice + 16 {
                        slice_count += 1;
                    } else if need == bit_slice + 1 {
                        slice_count += 2;
                    }
                }
            }
            if bit_count + slice_count >= BITPOOL {
                break;
            }
        }

        if bit_count + slice_count == BITPOOL {
            bit_count += slice_count;
            bit_slice -= 1;
        }

        for channel in 0..CHANNELS {
            for band in 0..SUBBANDS {
                let need = self.scale_factors[channel][band];
                self.allocated_bits[channel][band] = if need < bit_slice + 2 {
                    0
                } else {
                    (need - bit_slice).min(16)
                };
            }
        }

        let mut next_channel = 0;
        let mut next_band = 0;
        while bit_count < BITPOOL && next_band < SUBBANDS {
            let bits = self.allocated_bits[next_channel][next_band];
            if (2..16).contains(&bits) {
                self.allocated_bits[next_channel][next_band] += 1;
                bit_count += 1;
            } else if self.scale_factors[next_channel][next_band] == bit_slice + 1
                && bit_count + 1 < BITPOOL
            {
                self.allocated_bits[next_channel][next_band] = 2;
                bit_count += 2;
            }
            advance_cursor(&mut next_channel, &mut next_band);
        }

        next_channel = 0;
        next_band = 0;
        while bit_count < BITPOOL && next_band < SUBBANDS {
            if self.allocated_bits[next_channel][next_band] < 16 {
                self.allocated_bits[next_channel][next_band] += 1;
                bit_count += 1;
            }
            advance_cursor(&mut next_channel, &mut next_band);
        }
    }

    fn write_frame(&self) -> Result<[u8; SBC_FRAME_LENGTH]> {
        let mut output = [0u8; SBC_FRAME_LENGTH];
        let mut writer = BitWriter::new(&mut output);
        writer.put_bits(0x9c, 8);
        writer.put_bits(u32::from(self.configuration), 8);
        writer.put_bits(BITPOOL as u32, 8);
        writer.put_bits(0, 8);
        for band in 0..SUBBANDS - 1 {
            writer.put_bits(u32::from(self.joined[band]), 1);
        }
        writer.put_bits(0, 1);
        for channel in 0..CHANNELS {
            for band in 0..SUBBANDS {
                writer.put_bits(self.scale_factors[channel][band] as u32, 4);
            }
        }
        for block in 0..BLOCKS {
            for channel in 0..CHANNELS {
                for band in 0..SUBBANDS {
                    let bits = self.allocated_bits[channel][band];
                    if bits == 0 {
                        continue;
                    }
                    let levels = (1u32 << bits) - 1;
                    let limit = (1i32 << (self.scale_factors[channel][band] + 1)) as f64;
                    let normalized = self.subband_samples[channel][block * SUBBANDS + band] / limit;
                    let quantized = (((normalized + 1.0) * f64::from(levels) * 0.5) as i64)
                        .clamp(0, i64::from(levels));
                    writer.put_bits(quantized as u32, bits as u8);
                }
            }
        }
        writer.finish()?;
        output[3] = calculate_crc(&output);
        Ok(output)
    }
}

fn scale_factor(maximum: f64) -> i32 {
    let mut factor = 0;
    while factor < 15 && (1i32 << (factor + 1)) as f64 <= maximum {
        factor += 1;
    }
    factor
}

fn advance_cursor(channel: &mut usize, band: &mut usize) {
    *channel += 1;
    if *channel == CHANNELS {
        *channel = 0;
        *band += 1;
    }
}

fn calculate_crc(frame: &[u8; SBC_FRAME_LENGTH]) -> u8 {
    let mut crc = 0x0fu8;
    update_crc(&mut crc, frame[1]);
    update_crc(&mut crc, frame[2]);
    for byte in &frame[4..=12] {
        update_crc(&mut crc, *byte);
    }
    crc
}

fn update_crc(crc: &mut u8, value: u8) {
    for shift in (0..8).rev() {
        let high_bit = (((value >> shift) & 1) ^ (*crc >> 7)) != 0;
        *crc <<= 1;
        if high_bit {
            *crc ^= 0x1d;
        }
    }
}

struct BitWriter<'a> {
    buffer: &'a mut [u8],
    byte_offset: usize,
    bit_offset: u8,
    failed: bool,
}

impl<'a> BitWriter<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            byte_offset: 0,
            bit_offset: 0,
            failed: false,
        }
    }

    fn put_bits(&mut self, value: u32, count: u8) {
        if count == 0 || self.failed {
            return;
        }
        for shift in (0..count).rev() {
            if self.byte_offset >= self.buffer.len() {
                self.failed = true;
                return;
            }
            let mask = 1 << (7 - self.bit_offset);
            if (value >> shift) & 1 != 0 {
                self.buffer[self.byte_offset] |= mask;
            } else {
                self.buffer[self.byte_offset] &= !mask;
            }
            self.bit_offset += 1;
            if self.bit_offset == 8 {
                self.bit_offset = 0;
                self.byte_offset += 1;
            }
        }
    }

    fn finish(self) -> Result<()> {
        if self.failed {
            Err(invalid_input(
                "SBC frame exceeded its 109-byte output buffer",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_has_expected_header_and_crc() {
        let zeros = [0i16; SAMPLES_PER_FRAME];
        let mut encoder = DualShock4SbcEncoder::new(32_000).unwrap();
        let frame = encoder.encode(&zeros, &zeros).unwrap();
        assert_eq!(SBC_FRAME_LENGTH, frame.len());
        assert_eq!([0x9c, 0x7f, 48], frame[..3]);
        assert_eq!(calculate_crc(&frame), frame[3]);
    }

    #[test]
    fn sine_wave_has_nonzero_payload() {
        let mut left = [0i16; SAMPLES_PER_FRAME];
        let mut right = [0i16; SAMPLES_PER_FRAME];
        for index in 0..SAMPLES_PER_FRAME {
            let sample =
                (std::f64::consts::TAU * 1000.0 * index as f64 / 32_000.0).sin() * 16_000.0;
            left[index] = sample as i16;
            right[index] = sample as i16;
        }
        let mut encoder = DualShock4SbcEncoder::new(32_000).unwrap();
        let frame = encoder.encode(&left, &right).unwrap();
        assert!(frame[4..].iter().any(|byte| *byte != 0));
        assert_eq!(calculate_crc(&frame), frame[3]);
    }

    #[test]
    fn reset_restores_initial_history() {
        let left = [1000i16; SAMPLES_PER_FRAME];
        let right = [500i16; SAMPLES_PER_FRAME];
        let mut encoder = DualShock4SbcEncoder::new(32_000).unwrap();
        let first = encoder.encode(&left, &right).unwrap();
        encoder.encode(&left, &right).unwrap();
        encoder.reset();
        assert_eq!(first, encoder.encode(&left, &right).unwrap());
    }

    #[test]
    fn rejects_undersized_pcm() {
        let short = [0i16; 64];
        let valid = [0i16; SAMPLES_PER_FRAME];
        let mut encoder = DualShock4SbcEncoder::new(32_000).unwrap();
        assert!(encoder.encode(&short, &valid).is_err());
        assert!(encoder.encode(&valid, &short).is_err());
    }
}
