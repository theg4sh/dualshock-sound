use crate::protocol::{
    AUDIO_STREAM_REPORT_LENGTH, FRAMES_PER_REPORT, OutputRoute, SBC_FRAME_LENGTH,
    audio_control_report, stream_report,
};
use crate::{Result, invalid_input};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SONY_VENDOR_ID: u16 = 0x054c;
const DS4_PRODUCT_IDS: [u16; 3] = [0x05c4, 0x09cc, 0x05c5];
const O_NONBLOCK: i32 = 0x800;

#[derive(Clone, Debug)]
pub struct Controller {
    pub path: PathBuf,
    pub name: String,
    pub mac_address: String,
    pub product_id: u16,
}

pub fn discover() -> io::Result<Vec<Controller>> {
    let mut controllers = Vec::new();
    for entry in fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let uevent = entry.path().join("device/uevent");
        let Ok(contents) = fs::read_to_string(uevent) else {
            continue;
        };
        let Some((vendor_id, product_id, name, mac_address)) = parse_uevent(&contents) else {
            continue;
        };
        if vendor_id != SONY_VENDOR_ID || !DS4_PRODUCT_IDS.contains(&product_id) {
            continue;
        }
        controllers.push(Controller {
            path: Path::new("/dev").join(entry.file_name()),
            name,
            mac_address,
            product_id,
        });
    }
    controllers.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(controllers)
}

fn parse_uevent(contents: &str) -> Option<(u16, u16, String, String)> {
    let mut bus = None;
    let mut vendor_id = None;
    let mut product_id = None;
    let mut name = None;
    let mut mac_address = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("HID_ID=") {
            let mut fields = value.split(':');
            bus = Some(fields.next()?);
            vendor_id = u16::from_str_radix(fields.next()?, 16).ok();
            product_id = u16::from_str_radix(fields.next()?, 16).ok();
        } else if let Some(value) = line.strip_prefix("HID_NAME=") {
            name = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("HID_UNIQ=") {
            mac_address = Some(value.to_owned());
        }
    }
    if bus != Some("0005") {
        return None;
    }
    Some((vendor_id?, product_id?, name?, mac_address?))
}

pub fn select(device: Option<&Path>, controller: Option<&str>) -> Result<Controller> {
    if let Some(path) = device {
        return discover()?
            .into_iter()
            .find(|entry| entry.path == path)
            .ok_or_else(|| {
                invalid_input(format!(
                    "{} is not a Bluetooth DualShock 4; use --list-controllers",
                    path.display()
                ))
            });
    }
    let controllers = discover()?;
    if let Some(mac) = controller {
        return controllers
            .into_iter()
            .find(|entry| entry.mac_address.eq_ignore_ascii_case(mac))
            .ok_or_else(|| invalid_input(format!("Bluetooth DualShock 4 {mac} was not found")));
    }
    match controllers.len() {
        0 => Err(invalid_input(
            "no Bluetooth DualShock 4 hidraw device found; reconnect it and check hidp/hid_playstation",
        )),
        1 => Ok(controllers.into_iter().next().unwrap()),
        _ => Err(invalid_input(
            "multiple DualShock 4 controllers found; select one with --controller MAC",
        )),
    }
}

pub struct HidTransport {
    file: fs::File,
}

impl HidTransport {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NONBLOCK)
            .open(path)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot open {}: {error}", path.display()),
                )
            })?;
        Ok(Self { file })
    }

    pub fn arm(&mut self, route: OutputRoute, volume: u8) -> Result<()> {
        let (headphone_volume, speaker_volume) = match route {
            OutputRoute::Headphones => (volume, 0),
            OutputRoute::InternalSpeaker => (0, volume),
        };
        let report = audio_control_report(true, headphone_volume, speaker_volume, 0);
        self.write_report(&report)
    }

    pub fn disarm(&mut self) -> Result<()> {
        self.write_report(&audio_control_report(false, 0, 0, 0))
    }

    pub fn send(
        &mut self,
        frame_number: u16,
        frames: &[[u8; SBC_FRAME_LENGTH]; FRAMES_PER_REPORT],
        route: OutputRoute,
    ) -> Result<()> {
        self.write_report(&stream_report(frame_number, frames, route, 0))
    }

    fn write_report(&mut self, report: &[u8]) -> Result<()> {
        use std::io::Write;

        let deadline = Instant::now() + Duration::from_millis(40);
        loop {
            match self.file.write(report) {
                Ok(written) if written == report.len() => return Ok(()),
                Ok(written) => {
                    return Err(invalid_input(format!(
                        "short hidraw write: {} of {} bytes",
                        written,
                        report.len()
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "hidraw write timed out",
                        )
                        .into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub fn expected_stream_report_length() -> usize {
    AUDIO_STREAM_REPORT_LENGTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bluetooth_ds4_uevent() {
        let event = "HID_ID=0005:0000054C:000009CC\nHID_NAME=Wireless Controller\nHID_UNIQ=d0:bc:c1:a4:8e:05\n";
        assert_eq!(
            Some((
                0x054c,
                0x09cc,
                "Wireless Controller".into(),
                "d0:bc:c1:a4:8e:05".into()
            )),
            parse_uevent(event)
        );
    }

    #[test]
    fn rejects_usb_uevent_for_bluetooth_transport() {
        let event = "HID_ID=0003:0000054C:000009CC\nHID_NAME=Wireless Controller\nHID_UNIQ=\n";
        assert_eq!(None, parse_uevent(event));
    }
}
