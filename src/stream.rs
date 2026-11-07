use crate::capture;
use crate::hid::{self, HidTransport};
use crate::protocol::{
    FRAMES_PER_REPORT, OutputRoute, REPORT_CADENCE_MS, SAMPLE_RATE, SAMPLES_PER_FRAME,
    SBC_FRAME_LENGTH,
};
use crate::resampler::AudioResampler;
use crate::sbc::DualShock4SbcEncoder;
use crate::{Result, invalid_input};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_QUEUED_FRAMES: usize = 96;
const PRIME_TARGET_FRAMES: usize = 20;
const REPRIME_TARGET_FRAMES: usize = 12;

#[derive(Clone, Debug)]
pub struct StreamConfig {
    pub device: Option<PathBuf>,
    pub controller: Option<String>,
    pub source: Option<String>,
    pub route: OutputRoute,
    pub volume: u8,
    pub gain: f32,
    pub duration: Option<Duration>,
    pub test_tone: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            device: None,
            controller: None,
            source: None,
            route: OutputRoute::Headphones,
            volume: 60,
            gain: 1.0,
            duration: None,
            test_tone: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StreamStats {
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub frames_encoded: u64,
    pub underruns: u64,
    pub dropped_frames: u64,
}

pub fn run(config: StreamConfig, external_stop: &AtomicBool) -> Result<StreamStats> {
    if config.volume > 79 {
        return Err(invalid_input("hardware volume must be between 0 and 79"));
    }
    let controller = hid::select(config.device.as_deref(), config.controller.as_deref())?;
    eprintln!(
        "controller: {} ({}, {:04x}) at {}",
        controller.name,
        controller.mac_address,
        controller.product_id,
        controller.path.display()
    );
    let mut transport = HidTransport::open(&controller.path)?;
    transport.arm(config.route, config.volume)?;

    let stop = Arc::new(AtomicBool::new(false));
    let capture_error = Arc::new(Mutex::new(None::<String>));
    let (sender, receiver) = sync_channel::<Vec<f32>>(8);
    let worker = {
        let stop = Arc::clone(&stop);
        let capture_error = Arc::clone(&capture_error);
        let source = config.source.clone();
        let test_tone = config.test_tone;
        std::thread::spawn(move || {
            let result = if test_tone {
                capture::run_test_tone(stop, sender)
            } else {
                capture::run_capture(source, stop, sender)
            };
            if let Err(error) = result {
                *capture_error.lock().unwrap() = Some(error.to_string());
            }
        })
    };

    let result = transmit(
        &config,
        &mut transport,
        receiver,
        &stop,
        external_stop,
        &capture_error,
    );
    stop.store(true, Ordering::Relaxed);
    let disarm_result = transport.disarm();
    worker
        .join()
        .map_err(|_| invalid_input("audio worker panicked"))?;
    disarm_result?;
    result
}

fn transmit(
    config: &StreamConfig,
    transport: &mut HidTransport,
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    stop: &Arc<AtomicBool>,
    external_stop: &AtomicBool,
    capture_error: &Arc<Mutex<Option<String>>>,
) -> Result<StreamStats> {
    let started = Instant::now();
    let mut stats = StreamStats::default();
    let mut encoder = DualShock4SbcEncoder::new(SAMPLE_RATE)?;
    let silence = DualShock4SbcEncoder::silence();
    let mut resampler = AudioResampler::new(capture::CAPTURE_RATE, SAMPLE_RATE, config.gain)?;
    let mut pending_left = VecDeque::<i16>::with_capacity(4096);
    let mut pending_right = VecDeque::<i16>::with_capacity(4096);
    let mut queue = VecDeque::<[u8; SBC_FRAME_LENGTH]>::with_capacity(MAX_QUEUED_FRAMES);
    let mut frame_number = 0u16;
    let mut primed = false;
    let mut capture_disconnected = false;
    let mut next_send = Instant::now() + Duration::from_millis(REPORT_CADENCE_MS);
    let mut next_status = Instant::now() + Duration::from_secs(1);

    loop {
        if stop.load(Ordering::Relaxed)
            || external_stop.load(Ordering::Relaxed)
            || config
                .duration
                .is_some_and(|duration| started.elapsed() >= duration)
        {
            break;
        }
        if let Some(error) = capture_error.lock().unwrap().take() {
            return Err(invalid_input(error));
        }

        wait_until(next_send);
        next_send += Duration::from_millis(REPORT_CADENCE_MS);
        let now = Instant::now();
        if now > next_send + Duration::from_millis(2 * REPORT_CADENCE_MS) {
            next_send = now + Duration::from_millis(REPORT_CADENCE_MS);
        }

        pump_pcm(
            &receiver,
            &mut capture_disconnected,
            &mut resampler,
            &mut pending_left,
            &mut pending_right,
            &mut encoder,
            &mut queue,
            config.route,
            &mut stats,
        )?;

        let prime_target = if stats.packets_sent == 0 {
            PRIME_TARGET_FRAMES
        } else {
            REPRIME_TARGET_FRAMES
        };
        if !primed && queue.len() >= prime_target {
            primed = true;
        }

        let mut batch = [silence; FRAMES_PER_REPORT];
        if primed {
            let mut dequeued = 0;
            while dequeued < FRAMES_PER_REPORT {
                let Some(frame) = queue.pop_front() else {
                    break;
                };
                batch[dequeued] = frame;
                dequeued += 1;
            }
            if dequeued < FRAMES_PER_REPORT {
                stats.underruns += 1;
                primed = false;
            }
        }

        transport.send(frame_number, &batch, config.route)?;
        frame_number = frame_number.wrapping_add(FRAMES_PER_REPORT as u16);
        stats.packets_sent += 1;
        stats.bytes_sent += 462;

        if capture_disconnected && queue.is_empty() && !config.test_tone {
            return Err(invalid_input("desktop audio capture ended unexpectedly"));
        }
        if Instant::now() >= next_status {
            eprintln!(
                "streaming: {} packets, buffer {} frames, {} underruns, {} dropped",
                stats.packets_sent,
                queue.len(),
                stats.underruns,
                stats.dropped_frames
            );
            next_status = Instant::now() + Duration::from_secs(1);
        }
    }
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
fn pump_pcm(
    receiver: &std::sync::mpsc::Receiver<Vec<f32>>,
    disconnected: &mut bool,
    resampler: &mut AudioResampler,
    pending_left: &mut VecDeque<i16>,
    pending_right: &mut VecDeque<i16>,
    encoder: &mut DualShock4SbcEncoder,
    queue: &mut VecDeque<[u8; SBC_FRAME_LENGTH]>,
    route: OutputRoute,
    stats: &mut StreamStats,
) -> Result<()> {
    loop {
        let samples = match receiver.try_recv() {
            Ok(samples) => samples,
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                *disconnected = true;
                break;
            }
        };
        let estimated = samples.len() + 64;
        let mut left = vec![0i16; estimated];
        let mut right = vec![0i16; estimated];
        let count = resampler.resample(&samples, &mut left, &mut right);
        for index in 0..count {
            if route == OutputRoute::InternalSpeaker {
                let mono = ((i32::from(left[index]) + i32::from(right[index])) / 2) as i16;
                pending_left.push_back(mono);
                pending_right.push_back(mono);
            } else {
                pending_left.push_back(left[index]);
                pending_right.push_back(right[index]);
            }
        }
        while pending_left.len() >= SAMPLES_PER_FRAME && pending_right.len() >= SAMPLES_PER_FRAME {
            let mut left_frame = [0i16; SAMPLES_PER_FRAME];
            let mut right_frame = [0i16; SAMPLES_PER_FRAME];
            for index in 0..SAMPLES_PER_FRAME {
                left_frame[index] = pending_left.pop_front().unwrap();
                right_frame[index] = pending_right.pop_front().unwrap();
            }
            let encoded = encoder.encode(&left_frame, &right_frame)?;
            if queue.len() == MAX_QUEUED_FRAMES {
                queue.pop_front();
                stats.dropped_frames += 1;
            }
            queue.push_back(encoded);
            stats.frames_encoded += 1;
        }
    }
    Ok(())
}

fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        if remaining > Duration::from_millis(1) {
            std::thread::sleep(remaining - Duration::from_micros(500));
        } else {
            std::hint::spin_loop();
        }
    }
}
