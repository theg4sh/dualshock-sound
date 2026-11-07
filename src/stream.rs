use crate::capture;
use crate::fade::VolumeFade;
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
    pub node_latency: Option<String>,
    pub route: OutputRoute,
    pub volume: u8,
    pub gain: f32,
    pub duration: Option<Duration>,
    pub fade: Option<Duration>,
    pub test_tone: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            device: None,
            controller: None,
            source: None,
            node_latency: None,
            route: OutputRoute::Headphones,
            volume: 60,
            gain: 1.0,
            duration: None,
            fade: None,
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
    transport.arm(
        config.route,
        if config.fade.is_some_and(|fade| !fade.is_zero()) {
            0
        } else {
            config.volume
        },
    )?;

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
    let mut fade = VolumeFade::new(config.volume, config.fade.unwrap_or(Duration::ZERO));
    let mut stopping = false;
    let mut next_send = Instant::now() + Duration::from_millis(REPORT_CADENCE_MS);
    let mut next_status = Instant::now() + Duration::from_secs(1);

    loop {
        if stopping && fade.is_complete() {
            break;
        }
        if !stopping
            && (stop.load(Ordering::Relaxed)
                || external_stop.load(Ordering::Relaxed)
                || config
                    .duration
                    .is_some_and(|duration| started.elapsed() >= duration))
        {
            stopping = true;
            fade.set_target(0);
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
        if fade.tick() {
            transport.arm(config.route, fade.volume())?;
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

const ROUTE_ACTIVITY_HOLD: Duration = Duration::from_millis(300);

pub fn run_virtual(
    config: StreamConfig,
    external_stop: &'static AtomicBool,
) -> Result<StreamStats> {
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
    transport.arm(
        config.route,
        if config.fade.is_some_and(|fade| !fade.is_zero()) {
            0
        } else {
            config.volume
        },
    )?;

    let stop = Arc::new(AtomicBool::new(false));
    let (headphone_sender, headphone_receiver) = sync_channel::<Vec<f32>>(16);
    let (speaker_sender, speaker_receiver) = sync_channel::<Vec<f32>>(16);
    let worker = {
        let stop = Arc::clone(&stop);
        let worker_config = config.clone();
        std::thread::spawn(move || {
            let result = transmit_virtual(
                &worker_config,
                &mut transport,
                headphone_receiver,
                speaker_receiver,
                &stop,
            );
            stop.store(true, Ordering::Relaxed);
            let disarm_result = transport.disarm();
            disarm_result?;
            result
        })
    };

    let pipewire_result = crate::virtual_sinks::run(
        headphone_sender,
        speaker_sender,
        Arc::clone(&stop),
        external_stop,
        config.duration,
        config.node_latency.as_deref(),
    );
    stop.store(true, Ordering::Relaxed);
    let worker_result = worker
        .join()
        .map_err(|_| invalid_input("virtual-sink streaming worker panicked"))?;
    pipewire_result?;
    worker_result
}

struct VirtualRoute {
    route: OutputRoute,
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    resampler: AudioResampler,
    encoder: DualShock4SbcEncoder,
    pending_left: VecDeque<i16>,
    pending_right: VecDeque<i16>,
    queue: VecDeque<[u8; SBC_FRAME_LENGTH]>,
    last_activity: Option<Instant>,
    last_start: Option<Instant>,
    primed: bool,
    has_started: bool,
}

impl VirtualRoute {
    fn new(
        route: OutputRoute,
        receiver: std::sync::mpsc::Receiver<Vec<f32>>,
        gain: f32,
    ) -> Result<Self> {
        Ok(Self {
            route,
            receiver,
            resampler: AudioResampler::new(capture::CAPTURE_RATE, SAMPLE_RATE, gain)?,
            encoder: DualShock4SbcEncoder::new(SAMPLE_RATE)?,
            pending_left: VecDeque::with_capacity(4096),
            pending_right: VecDeque::with_capacity(4096),
            queue: VecDeque::with_capacity(MAX_QUEUED_FRAMES),
            last_activity: None,
            last_start: None,
            primed: false,
            has_started: false,
        })
    }

    fn pump(&mut self, stats: &mut StreamStats) -> Result<()> {
        loop {
            let samples = match self.receiver.try_recv() {
                Ok(samples) => samples,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            };
            let now = Instant::now();
            if samples.iter().any(|sample| sample.abs() > 0.00001) {
                let was_active = self.is_active(now);
                if !was_active {
                    self.last_start = Some(now);
                }
                self.last_activity = Some(now);
            }

            let estimated = samples.len() + 64;
            let mut left = vec![0i16; estimated];
            let mut right = vec![0i16; estimated];
            let count = self.resampler.resample(&samples, &mut left, &mut right);
            for index in 0..count {
                if self.route == OutputRoute::InternalSpeaker {
                    let mono = ((i32::from(left[index]) + i32::from(right[index])) / 2) as i16;
                    self.pending_left.push_back(mono);
                    self.pending_right.push_back(mono);
                } else {
                    self.pending_left.push_back(left[index]);
                    self.pending_right.push_back(right[index]);
                }
            }
            while self.pending_left.len() >= SAMPLES_PER_FRAME
                && self.pending_right.len() >= SAMPLES_PER_FRAME
            {
                let mut left_frame = [0i16; SAMPLES_PER_FRAME];
                let mut right_frame = [0i16; SAMPLES_PER_FRAME];
                for index in 0..SAMPLES_PER_FRAME {
                    left_frame[index] = self.pending_left.pop_front().unwrap();
                    right_frame[index] = self.pending_right.pop_front().unwrap();
                }
                let encoded = self.encoder.encode(&left_frame, &right_frame)?;
                if self.queue.len() == MAX_QUEUED_FRAMES {
                    self.queue.pop_front();
                    stats.dropped_frames += 1;
                }
                self.queue.push_back(encoded);
                stats.frames_encoded += 1;
            }
        }
        Ok(())
    }

    fn is_active(&self, now: Instant) -> bool {
        self.last_activity
            .is_some_and(|last| now.saturating_duration_since(last) <= ROUTE_ACTIVITY_HOLD)
    }

    fn clear_buffers(&mut self) {
        self.pending_left.clear();
        self.pending_right.clear();
        self.queue.clear();
        self.encoder.reset();
        self.primed = false;
    }

    fn next_batch(
        &mut self,
        silence: [u8; SBC_FRAME_LENGTH],
        stats: &mut StreamStats,
    ) -> [[u8; SBC_FRAME_LENGTH]; FRAMES_PER_REPORT] {
        let target = if self.has_started {
            REPRIME_TARGET_FRAMES
        } else {
            PRIME_TARGET_FRAMES
        };
        if !self.primed && self.queue.len() >= target {
            self.primed = true;
            self.has_started = true;
        }
        let mut batch = [silence; FRAMES_PER_REPORT];
        if !self.primed {
            return batch;
        }
        let mut dequeued = 0;
        while dequeued < FRAMES_PER_REPORT {
            let Some(frame) = self.queue.pop_front() else {
                break;
            };
            batch[dequeued] = frame;
            dequeued += 1;
        }
        if dequeued < FRAMES_PER_REPORT {
            stats.underruns += 1;
            self.primed = false;
        }
        batch
    }
}

fn transmit_virtual(
    config: &StreamConfig,
    transport: &mut HidTransport,
    headphone_receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    speaker_receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    stop: &Arc<AtomicBool>,
) -> Result<StreamStats> {
    let started = Instant::now();
    let mut stats = StreamStats::default();
    let silence = DualShock4SbcEncoder::silence();
    let mut headphones =
        VirtualRoute::new(OutputRoute::Headphones, headphone_receiver, config.gain)?;
    let mut speaker =
        VirtualRoute::new(OutputRoute::InternalSpeaker, speaker_receiver, config.gain)?;
    let mut active_route = config.route;
    let mut fade = VolumeFade::new(config.volume, config.fade.unwrap_or(Duration::ZERO));
    fade.set_target(0);
    let mut stopping = false;
    let mut buffers_cleared = false;
    let mut frame_number = 0u16;
    let mut next_send = Instant::now() + Duration::from_millis(REPORT_CADENCE_MS);
    let mut next_status = Instant::now() + Duration::from_secs(1);

    loop {
        if stopping && fade.is_complete() {
            break;
        }
        if !stopping
            && (stop.load(Ordering::Relaxed)
                || config
                    .duration
                    .is_some_and(|duration| started.elapsed() >= duration))
        {
            stopping = true;
            fade.set_target(0);
        }
        wait_until(next_send);
        next_send += Duration::from_millis(REPORT_CADENCE_MS);
        let now = Instant::now();
        if now > next_send + Duration::from_millis(2 * REPORT_CADENCE_MS) {
            next_send = now + Duration::from_millis(REPORT_CADENCE_MS);
        }

        headphones.pump(&mut stats)?;
        speaker.pump(&mut stats)?;
        let next_route = select_virtual_route(&headphones, &speaker, active_route, now);
        let route_changed = next_route != active_route;
        if route_changed {
            active_route = next_route;
            match active_route {
                OutputRoute::Headphones => speaker.clear_buffers(),
                OutputRoute::InternalSpeaker => headphones.clear_buffers(),
            }
            eprintln!("active DS4 route: {active_route:?}");
        }

        let idle = !headphones.is_active(now) && !speaker.is_active(now);
        if !stopping {
            fade.set_target(if idle { 0 } else { config.volume });
        }
        let volume_changed = fade.tick();
        if route_changed || volume_changed {
            transport.arm(active_route, fade.volume())?;
        }

        let batch = if idle && fade.is_complete() {
            if !buffers_cleared {
                headphones.clear_buffers();
                speaker.clear_buffers();
                buffers_cleared = true;
            }
            [silence; FRAMES_PER_REPORT]
        } else {
            buffers_cleared = false;
            match active_route {
                OutputRoute::Headphones => headphones.next_batch(silence, &mut stats),
                OutputRoute::InternalSpeaker => speaker.next_batch(silence, &mut stats),
            }
        };
        transport.send(frame_number, &batch, active_route)?;
        frame_number = frame_number.wrapping_add(FRAMES_PER_REPORT as u16);
        stats.packets_sent += 1;
        stats.bytes_sent += 462;

        if Instant::now() >= next_status {
            let buffered = match active_route {
                OutputRoute::Headphones => headphones.queue.len(),
                OutputRoute::InternalSpeaker => speaker.queue.len(),
            };
            eprintln!(
                "streaming: {active_route:?}, {} packets, buffer {} frames, {} underruns, {} dropped",
                stats.packets_sent, buffered, stats.underruns, stats.dropped_frames
            );
            next_status = Instant::now() + Duration::from_secs(1);
        }
    }
    Ok(stats)
}

fn select_virtual_route(
    headphones: &VirtualRoute,
    speaker: &VirtualRoute,
    current: OutputRoute,
    now: Instant,
) -> OutputRoute {
    let headphones_active = headphones.is_active(now);
    let speaker_active = speaker.is_active(now);
    match current {
        OutputRoute::Headphones if speaker_active => {
            if !headphones_active || speaker.last_start > headphones.last_start {
                OutputRoute::InternalSpeaker
            } else {
                current
            }
        }
        OutputRoute::InternalSpeaker if headphones_active => {
            if !speaker_active || headphones.last_start > speaker.last_start {
                OutputRoute::Headphones
            } else {
                current
            }
        }
        _ => current,
    }
}

#[cfg(test)]
mod virtual_route_tests {
    use super::*;

    fn route(route: OutputRoute) -> VirtualRoute {
        let (_sender, receiver) = sync_channel(1);
        VirtualRoute::new(route, receiver, 1.0).unwrap()
    }

    fn activate(route: &mut VirtualRoute, at: Instant) {
        route.last_activity = Some(at);
        route.last_start = Some(at);
    }

    #[test]
    fn idle_sinks_keep_the_current_route() {
        let now = Instant::now();
        let headphones = route(OutputRoute::Headphones);
        let speaker = route(OutputRoute::InternalSpeaker);
        assert_eq!(
            OutputRoute::InternalSpeaker,
            select_virtual_route(&headphones, &speaker, OutputRoute::InternalSpeaker, now)
        );
    }

    #[test]
    fn newly_active_sink_takes_over() {
        let now = Instant::now();
        let mut headphones = route(OutputRoute::Headphones);
        let speaker = route(OutputRoute::InternalSpeaker);
        activate(&mut headphones, now);
        assert_eq!(
            OutputRoute::Headphones,
            select_virtual_route(&headphones, &speaker, OutputRoute::InternalSpeaker, now)
        );
    }

    #[test]
    fn simultaneous_continuous_audio_does_not_flap() {
        let now = Instant::now();
        let mut headphones = route(OutputRoute::Headphones);
        let mut speaker = route(OutputRoute::InternalSpeaker);
        activate(&mut headphones, now - Duration::from_millis(200));
        activate(&mut speaker, now - Duration::from_millis(100));
        assert_eq!(
            OutputRoute::InternalSpeaker,
            select_virtual_route(&headphones, &speaker, OutputRoute::InternalSpeaker, now)
        );
    }

    #[test]
    fn restarted_sink_takes_over_from_continuous_audio() {
        let now = Instant::now();
        let mut headphones = route(OutputRoute::Headphones);
        let mut speaker = route(OutputRoute::InternalSpeaker);
        activate(&mut headphones, now - Duration::from_millis(50));
        activate(&mut speaker, now - Duration::from_millis(200));
        assert_eq!(
            OutputRoute::Headphones,
            select_virtual_route(&headphones, &speaker, OutputRoute::InternalSpeaker, now)
        );
    }
}
