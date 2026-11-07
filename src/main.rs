use ds4_sound_link::protocol::OutputRoute;
use ds4_sound_link::stream::{StreamConfig, run, run_virtual};
use ds4_sound_link::{Result, invalid_input};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_signal(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

unsafe extern "C" {
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
}

fn main() {
    if let Err(error) = entry() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn entry() -> Result<()> {
    let Some(config) = parse_arguments()? else {
        return Ok(());
    };
    unsafe {
        signal(2, stop_signal);
        signal(15, stop_signal);
    }
    let direct_mode =
        config.test_tone || config.source.is_some() || config.route == OutputRoute::InternalSpeaker;
    let stats = if direct_mode {
        run(config, &STOP)?
    } else {
        run_virtual(config, &STOP)?
    };
    println!(
        "stopped: {} packets, {} SBC frames, {} underruns, {} dropped frames",
        stats.packets_sent, stats.frames_encoded, stats.underruns, stats.dropped_frames
    );
    Ok(())
}

fn parse_arguments() -> Result<Option<StreamConfig>> {
    let mut config = StreamConfig::default();
    let mut list_controllers = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "--list-controllers" => list_controllers = true,
            "--device" => config.device = Some(PathBuf::from(value(&mut arguments, "--device")?)),
            "--controller" => config.controller = Some(value(&mut arguments, "--controller")?),
            "--source" => config.source = Some(value(&mut arguments, "--source")?),
            "--speaker" => config.route = OutputRoute::InternalSpeaker,
            "--volume" => {
                let raw = value(&mut arguments, "--volume")?;
                config.volume = raw.parse()?;
                if config.volume > 79 {
                    return Err(invalid_input("--volume must be between 0 and 79"));
                }
            }
            "--gain" => {
                let raw = value(&mut arguments, "--gain")?;
                config.gain = raw.parse()?;
                if !(0.5..=2.5).contains(&config.gain) {
                    return Err(invalid_input("--gain must be between 0.5 and 2.5"));
                }
            }
            "--fade" => {
                let seconds = value(&mut arguments, "--fade")?.parse::<f64>()?;
                config.fade = Some(Duration::try_from_secs_f64(seconds).map_err(|_| {
                    invalid_input("--fade must be a finite, nonnegative number of seconds")
                })?);
            }
            "--duration" => {
                let seconds = value(&mut arguments, "--duration")?.parse::<u64>()?;
                if seconds == 0 {
                    return Err(invalid_input("--duration must be at least one second"));
                }
                config.duration = Some(Duration::from_secs(seconds));
            }
            "--test-tone" => config.test_tone = true,
            _ => return Err(invalid_input(format!("unknown argument: {argument}"))),
        }
    }
    if list_controllers {
        let controllers = ds4_sound_link::hid::discover()?;
        if controllers.is_empty() {
            println!("No Bluetooth DualShock 4 controllers found.");
        } else {
            for controller in controllers {
                println!(
                    "{}  {:04x}  {}  {}",
                    controller.path.display(),
                    controller.product_id,
                    controller.mac_address,
                    controller.name
                );
            }
        }
        return Ok(None);
    }
    Ok(Some(config))
}

fn value(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    arguments
        .next()
        .ok_or_else(|| invalid_input(format!("{option} requires a value")))
}

fn print_help() {
    println!(
        "ds4-sound-link — stream Linux desktop audio to a Bluetooth DualShock 4\n\
         \nUSAGE:\n\
         \x20   ds4-sound-link [OPTIONS]\n\
         \nOPTIONS:\n\
         \x20   --list-controllers      List Bluetooth DS4 hidraw devices\n\
         \x20   --controller MAC        Select a controller by Bluetooth address\n\
         \x20   --device PATH           Select a hidraw device explicitly\n\
         \x20   --source NAME           Capture an existing monitor directly instead of\n\
         \x20                           creating the two virtual DS4 output devices\n\
         \x20   --speaker               Direct internal-speaker mode instead of virtual devices\n\
         \x20   --volume 0..79          DS4 hardware volume (default: 60)\n\
         \x20   --gain 0.5..2.5         Software capture gain (default: 1.0)\n\
         \x20   --fade SECONDS          Enable start/stop fading (for example: --fade 0.5)\n\
         \x20   --duration SECONDS      Stop automatically after a duration\n\
         \x20   --test-tone             Send a quiet 440 Hz tone instead of capturing audio\n\
         \x20   -h, --help              Show this help\n\
         \nList monitor sources with: pactl list short sources"
    );
}
