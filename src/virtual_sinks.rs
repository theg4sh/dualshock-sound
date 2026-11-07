use crate::Result;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::format_utils;
use pw::spa::pod::Pod;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

pub const HEADPHONES_SINK: &str = "ds4_sound_link_headphones";
pub const SPEAKER_SINK: &str = "ds4_sound_link_speaker";

struct SinkData {
    format: spa::param::audio::AudioInfoRaw,
    sender: SyncSender<Vec<f32>>,
    stop: Arc<AtomicBool>,
}

pub fn run(
    headphone_sender: SyncSender<Vec<f32>>,
    speaker_sender: SyncSender<Vec<f32>>,
    stop: Arc<AtomicBool>,
    external_stop: &'static AtomicBool,
    duration: Option<Duration>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let (_headphones, _headphone_listener) = create_sink(
        &core,
        HEADPHONES_SINK,
        "DS4 Headphones",
        headphone_sender,
        Arc::clone(&stop),
    )?;
    let (_speaker, _speaker_listener) = create_sink(
        &core,
        SPEAKER_SINK,
        "DS4 Speaker",
        speaker_sender,
        Arc::clone(&stop),
    )?;

    let started = Instant::now();
    let timer_loop = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop.load(Ordering::Relaxed)
            || external_stop.load(Ordering::Relaxed)
            || duration.is_some_and(|limit| started.elapsed() >= limit)
        {
            timer_loop.quit();
        }
    });
    timer.update_timer(
        Some(Duration::from_millis(50)),
        Some(Duration::from_millis(50)),
    );

    eprintln!("PipeWire sinks ready: DS4 Headphones, DS4 Speaker");
    mainloop.run();
    Ok(())
}

fn create_sink<'c>(
    core: &'c pw::core::Core,
    node_name: &str,
    description: &str,
    sender: SyncSender<Vec<f32>>,
    stop: Arc<AtomicBool>,
) -> Result<(
    pw::stream::StreamBox<'c>,
    pw::stream::StreamListener<SinkData>,
)> {
    let stream = pw::stream::StreamBox::new(
        core,
        node_name,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => node_name,
            *pw::keys::NODE_DESCRIPTION => description,
            *pw::keys::NODE_VIRTUAL => "true",
            "audio.rate" => "48000",
            *pw::keys::AUDIO_CHANNELS => "2",
            "audio.position" => "FL,FR",
        },
    )?;
    let data = SinkData {
        format: Default::default(),
        sender,
        stop,
    };
    let listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type == MediaType::Audio && media_subtype == MediaSubtype::Raw {
                data.format.parse(param).expect("invalid raw audio format");
            }
        })
        .state_changed(|_, data, _, new| {
            if matches!(new, pw::stream::StreamState::Error(_)) {
                data.stop.store(true, Ordering::Relaxed);
            }
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(buffer_data) = datas.first_mut() else {
                return;
            };
            let sample_bytes = buffer_data.chunk().size() as usize;
            let Some(bytes) = buffer_data.data() else {
                return;
            };
            if data.format.channels() != 2 || data.format.rate() != 48_000 {
                return;
            }
            let sample_count = sample_bytes.min(bytes.len()) / size_of::<f32>();
            let samples: Vec<f32> = bytes[..sample_count * size_of::<f32>()]
                .chunks_exact(size_of::<f32>())
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                .collect();
            let _ = data.sender.try_send(samples);
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(48_000);
    audio_info.set_channels(2);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;
    Ok((stream, listener))
}
