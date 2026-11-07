use crate::{Result, invalid_input};

pub const AUDIO_CONTROL_REPORT_LENGTH: usize = 78;
pub const AUDIO_STREAM_REPORT_LENGTH: usize = 462;
pub const SAMPLE_RATE: u32 = 32_000;
pub const SAMPLES_PER_FRAME: usize = 128;
pub const SBC_FRAME_LENGTH: usize = 109;
pub const FRAMES_PER_REPORT: usize = 4;
pub const REPORT_CADENCE_MS: u64 = 16;
pub const MAX_HARDWARE_VOLUME: u8 = 0x4f;

const OUTPUT_REPORT_PREFIX: u8 = 0xa2;
const AUDIO_MODE_ENABLED: u8 = 0xa0;
const AUDIO_VOLUME_FLAGS: u8 = 0x10 | 0x20 | 0x80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputRoute {
    Headphones,
    InternalSpeaker,
}

impl OutputRoute {
    fn target(self) -> u8 {
        match self {
            Self::Headphones => 0x24,
            Self::InternalSpeaker => 0x02,
        }
    }
}

pub fn scale_to_hardware_volume(volume: u8) -> u8 {
    if volume == 0 {
        return 0;
    }
    ((volume.min(127) as f64 * (79.0 / 127.0)).round_ties_even() as u8).min(MAX_HARDWARE_VOLUME)
}

pub fn bluetooth_crc32(data: &[u8]) -> u32 {
    crc32(OUTPUT_REPORT_PREFIX, data)
}

pub fn crc32(prefix: u8, data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in std::iter::once(&prefix).chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

pub fn append_checksum(report: &mut [u8]) -> Result<()> {
    if report.len() < 4 {
        return Err(invalid_input("report must have space for a four-byte CRC"));
    }
    let data_length = report.len() - 4;
    let crc = bluetooth_crc32(&report[..data_length]);
    report[data_length..].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

pub fn valid_checksum(report: &[u8]) -> bool {
    if report.len() < 4 {
        return false;
    }
    let data_length = report.len() - 4;
    let expected = u32::from_le_bytes(report[data_length..].try_into().unwrap());
    bluetooth_crc32(&report[..data_length]) == expected
}

pub fn audio_control_report(
    enabled: bool,
    headphone_volume: u8,
    speaker_volume: u8,
    bluetooth_poll_rate: u8,
) -> [u8; AUDIO_CONTROL_REPORT_LENGTH] {
    let mut report = [0u8; AUDIO_CONTROL_REPORT_LENGTH];
    report[0] = 0x11;
    report[1] = 0xc0 | bluetooth_poll_rate.min(16);
    report[2] = AUDIO_MODE_ENABLED;
    report[3] = if enabled { AUDIO_VOLUME_FLAGS } else { 0 };
    report[21] = if enabled { headphone_volume } else { 0 };
    report[22] = report[21];
    report[24] = if enabled { speaker_volume } else { 0 };
    append_checksum(&mut report).unwrap();
    report
}

pub fn stream_report(
    frame_number: u16,
    frames: &[[u8; SBC_FRAME_LENGTH]; FRAMES_PER_REPORT],
    route: OutputRoute,
    bluetooth_poll_rate: u8,
) -> [u8; AUDIO_STREAM_REPORT_LENGTH] {
    let mut report = [0u8; AUDIO_STREAM_REPORT_LENGTH];
    report[0] = 0x17;
    report[1] = 0x40 | bluetooth_poll_rate.min(16);
    report[2] = AUDIO_MODE_ENABLED;
    report[3..5].copy_from_slice(&frame_number.to_le_bytes());
    report[5] = route.target();
    for (index, frame) in frames.iter().enumerate() {
        let offset = 6 + index * SBC_FRAME_LENGTH;
        report[offset..offset + SBC_FRAME_LENGTH].copy_from_slice(frame);
    }
    append_checksum(&mut report).unwrap();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_crc_includes_the_report_prefix() {
        let data = b"123456789";
        let mut expected = 0xffff_ffffu32;
        for byte in std::iter::once(&0xa2).chain(data) {
            expected ^= u32::from(*byte);
            for _ in 0..8 {
                expected = (expected >> 1) ^ if expected & 1 != 0 { 0xedb8_8320 } else { 0 };
            }
        }
        assert_eq!(!expected, bluetooth_crc32(data));
    }

    #[test]
    fn enabled_control_report_arms_audio_and_has_valid_crc() {
        let report = audio_control_report(true, 60, 0, 0);
        assert_eq!(0x11, report[0]);
        assert_eq!(0xa0, report[2]);
        assert_eq!(0xb0, report[3]);
        assert_eq!(60, report[21]);
        assert_eq!(60, report[22]);
        assert_eq!(0, report[24]);
        assert_eq!(0, report[25]);
        assert!(valid_checksum(&report));
    }

    #[test]
    fn disabled_control_report_preserves_normal_mode_and_effects() {
        let report = audio_control_report(false, 60, 60, 0);
        assert_eq!(0xa0, report[2]);
        assert_eq!(0, report[3]);
        assert_eq!(0, report[21]);
        assert_eq!(0, report[22]);
        assert_eq!(0, report[24]);
        assert_eq!(0, report[25]);
        assert!(valid_checksum(&report));
    }

    #[test]
    fn stream_report_contains_four_frames_and_valid_crc() {
        let mut frames = [[0u8; SBC_FRAME_LENGTH]; FRAMES_PER_REPORT];
        for (index, frame) in frames.iter_mut().enumerate() {
            frame[0] = 0x9c;
            frame[1] = index as u8 + 1;
        }
        let report = stream_report(0x1234, &frames, OutputRoute::Headphones, 0);
        assert_eq!(0x17, report[0]);
        assert_eq!(0x40, report[1]);
        assert_eq!(0xa0, report[2]);
        assert_eq!([0x34, 0x12], report[3..5]);
        assert_eq!(0x24, report[5]);
        for index in 0..4 {
            let offset = 6 + index * SBC_FRAME_LENGTH;
            assert_eq!(0x9c, report[offset]);
            assert_eq!(index as u8 + 1, report[offset + 1]);
        }
        assert!(valid_checksum(&report));
    }

    #[test]
    fn speaker_route_uses_internal_target() {
        let frames = [[0u8; SBC_FRAME_LENGTH]; FRAMES_PER_REPORT];
        let report = stream_report(1, &frames, OutputRoute::InternalSpeaker, 0);
        assert_eq!(0x02, report[5]);
    }

    #[test]
    fn hardware_volume_is_scaled_and_clamped() {
        assert_eq!(0, scale_to_hardware_volume(0));
        assert_eq!(40, scale_to_hardware_volume(64));
        assert_eq!(MAX_HARDWARE_VOLUME, scale_to_hardware_volume(127));
        assert_eq!(MAX_HARDWARE_VOLUME, scale_to_hardware_volume(255));
    }
}
