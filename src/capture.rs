use crate::{Result, invalid_input};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

pub const CAPTURE_RATE: u32 = 48_000;
pub const CAPTURE_FRAMES: usize = 480;
const PA_STREAM_RECORD: c_int = 2;
const PA_SAMPLE_FLOAT32LE: c_int = 5;

#[repr(C)]
struct PaSampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct PaBufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

#[repr(C)]
struct PaSimple {
    _private: [u8; 0],
}

#[link(name = "pulse-simple")]
unsafe extern "C" {
    fn pa_simple_new(
        server: *const c_char,
        name: *const c_char,
        direction: c_int,
        device: *const c_char,
        stream_name: *const c_char,
        sample_spec: *const PaSampleSpec,
        channel_map: *const c_void,
        buffer_attr: *const PaBufferAttr,
        error: *mut c_int,
    ) -> *mut PaSimple;
    fn pa_simple_read(
        stream: *mut PaSimple,
        data: *mut c_void,
        bytes: usize,
        error: *mut c_int,
    ) -> c_int;
    fn pa_simple_free(stream: *mut PaSimple);
}

#[link(name = "pulse")]
unsafe extern "C" {
    fn pa_strerror(error: c_int) -> *const c_char;
}

struct PulseCapture {
    stream: *mut PaSimple,
}

impl PulseCapture {
    fn connect(source: Option<&str>) -> Result<Self> {
        let application = CString::new("DS4 Sound Link").unwrap();
        let stream_name = CString::new("Desktop monitor capture").unwrap();
        let source = CString::new(source.unwrap_or("@DEFAULT_MONITOR@"))?;
        let sample_spec = PaSampleSpec {
            format: PA_SAMPLE_FLOAT32LE,
            rate: CAPTURE_RATE,
            channels: 2,
        };
        let buffer_attr = PaBufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: (CAPTURE_FRAMES * 2 * size_of::<f32>()) as u32,
        };
        let mut error = 0;
        let stream = unsafe {
            pa_simple_new(
                ptr::null(),
                application.as_ptr(),
                PA_STREAM_RECORD,
                source.as_ptr(),
                stream_name.as_ptr(),
                &sample_spec,
                ptr::null(),
                &buffer_attr,
                &mut error,
            )
        };
        if stream.is_null() {
            return Err(invalid_input(format!(
                "PulseAudio/PipeWire capture failed: {}",
                pulse_error(error)
            )));
        }
        Ok(Self { stream })
    }

    fn read(&mut self, buffer: &mut [f32]) -> Result<()> {
        let mut error = 0;
        let result = unsafe {
            pa_simple_read(
                self.stream,
                buffer.as_mut_ptr().cast(),
                size_of_val(buffer),
                &mut error,
            )
        };
        if result < 0 {
            Err(invalid_input(format!(
                "PulseAudio/PipeWire read failed: {}",
                pulse_error(error)
            )))
        } else {
            Ok(())
        }
    }
}

impl Drop for PulseCapture {
    fn drop(&mut self) {
        unsafe { pa_simple_free(self.stream) }
    }
}

fn pulse_error(error: c_int) -> String {
    let message = unsafe { pa_strerror(error) };
    if message.is_null() {
        format!("error {error}")
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

pub fn run_capture(
    source: Option<String>,
    stop: Arc<AtomicBool>,
    sender: SyncSender<Vec<f32>>,
) -> Result<()> {
    let mut capture = PulseCapture::connect(source.as_deref())?;
    let mut samples = vec![0.0f32; CAPTURE_FRAMES * 2];
    while !stop.load(Ordering::Relaxed) {
        capture.read(&mut samples)?;
        if sender.send(samples.clone()).is_err() {
            break;
        }
    }
    Ok(())
}

pub fn run_test_tone(stop: Arc<AtomicBool>, sender: SyncSender<Vec<f32>>) -> Result<()> {
    let mut sample_clock = 0u64;
    let mut next_chunk = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let mut samples = vec![0.0f32; CAPTURE_FRAMES * 2];
        for frame in 0..CAPTURE_FRAMES {
            let time = sample_clock as f32 / CAPTURE_RATE as f32;
            let sample = (std::f32::consts::TAU * 440.0 * time).sin() * 0.20;
            samples[frame * 2] = sample;
            samples[frame * 2 + 1] = sample;
            sample_clock += 1;
        }
        if sender.send(samples).is_err() {
            break;
        }
        next_chunk += Duration::from_millis(10);
        let now = Instant::now();
        if next_chunk > now {
            std::thread::sleep(next_chunk - now);
        } else {
            next_chunk = now;
        }
    }
    Ok(())
}
