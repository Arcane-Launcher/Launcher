//! Recorder sound: WASAPI capture threads (game process loopback, whole-PC
//! loopback, microphone) feeding queues, and a mixer that turns them into
//! one 48 kHz 16-bit stereo stream on the video's clock.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;

use super::sink::{SAMPLE_RATE, TICKS_PER_SEC};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Stereo frames.
type Queue = Arc<Mutex<VecDeque<[i16; 2]>>>;

/// Audio is written this far behind the video clock so packets that are
/// still on their way are in the queue when their turn comes.
pub const LATENCY_FRAMES: u64 = SAMPLE_RATE as u64 / 10;
/// A source with more than this buffered beyond what's due is dropping
/// behind real time (e.g. no video frames while minimized); the oldest
/// sound is discarded to stay in sync.
const MAX_BACKLOG: usize = SAMPLE_RATE as usize * 3 / 10;
/// Hard cap on a queue, whatever happens.
const QUEUE_LIMIT: usize = SAMPLE_RATE as usize * 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    /// The game's process tree only.
    Game(u32),
    /// Everything played through the default output device.
    System,
    /// A microphone; `None` = the Windows default.
    Mic,
}

#[derive(Debug, Clone, Serialize)]
pub struct Microphone {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn list_microphones() -> Vec<Microphone> {
    let _ = wasapi::initialize_mta();
    let Ok(enumerator) = wasapi::DeviceEnumerator::new() else {
        return Vec::new();
    };
    let default_id = enumerator
        .get_default_device(&wasapi::Direction::Capture)
        .ok()
        .and_then(|d| d.get_id().ok());
    let Ok(collection) = enumerator.get_device_collection(&wasapi::Direction::Capture) else {
        return Vec::new();
    };
    let count = collection.get_nbr_devices().unwrap_or(0);
    (0..count)
        .filter_map(|i| collection.get_device_at_index(i).ok())
        .filter_map(|d| {
            let id = d.get_id().ok()?;
            Some(Microphone {
                is_default: default_id.as_deref() == Some(id.as_str()),
                name: d.get_friendlyname().unwrap_or_else(|_| "Microphone".into()),
                id,
            })
        })
        .collect()
}

struct Source {
    queue: Queue,
    gain: f32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Source {
    fn start(kind: SourceKind, mic_device: Option<String>, gain: f32) -> Result<Self, String> {
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (q, s) = (queue.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("recorder-audio".into())
            .spawn(move || {
                if let Err(e) = run(kind, mic_device, &q, &s, &ready_tx) {
                    let _ = ready_tx.send(Err(e.to_string()));
                    log::error!("recorder audio ({kind:?}) stopped: {e}");
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                queue,
                gain,
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err("the audio device didn't respond".into())
            }
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    kind: SourceKind,
    mic_device: Option<String>,
    queue: &Mutex<VecDeque<[i16; 2]>>,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<(), BoxError> {
    use wasapi::{AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

    let _ = wasapi::initialize_mta();
    let format = WaveFormat::new(16, 16, &SampleType::Int, SAMPLE_RATE as usize, 2, None);
    let mut client = match kind {
        SourceKind::Game(pid) => AudioClient::new_application_loopback_client(pid, true)?,
        SourceKind::System => DeviceEnumerator::new()?
            .get_default_device(&Direction::Render)?
            .get_iaudioclient()?,
        SourceKind::Mic => {
            let enumerator = DeviceEnumerator::new()?;
            let device = match &mic_device {
                // Falls back to the default if the chosen one was unplugged.
                Some(id) => enumerator
                    .get_device(id)
                    .or_else(|_| enumerator.get_default_device(&Direction::Capture))?,
                None => enumerator.get_default_device(&Direction::Capture)?,
            };
            device.get_iaudioclient()?
        }
    };
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    client.initialize_client(&format, &Direction::Capture, &mode)?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    let _ = ready.send(Ok(()));

    let mut buf = vec![0u8; SAMPLE_RATE as usize * 4];
    while !stop.load(Ordering::Relaxed) {
        // Times out during silence (loopback sends nothing); fine.
        let _ = event.wait_for_event(100);
        while let Some(frames) = capture.get_next_packet_size()? {
            if frames == 0 {
                break;
            }
            let needed = frames as usize * 4;
            if buf.len() < needed {
                buf.resize(needed, 0);
            }
            let (read, info) = capture.read_from_device(&mut buf)?;
            let bytes = &buf[..read as usize * 4];
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            if info.flags.silent {
                q.extend(std::iter::repeat_n([0i16; 2], read as usize));
            } else {
                q.extend(bytes.chunks_exact(4).map(|c| {
                    [
                        i16::from_le_bytes([c[0], c[1]]),
                        i16::from_le_bytes([c[2], c[3]]),
                    ]
                }));
            }
            if q.len() > QUEUE_LIMIT {
                let excess = q.len() - QUEUE_LIMIT;
                q.drain(..excess);
            }
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Mixes every source onto one timeline counted in sample frames from the
/// recording session's start.
pub struct Mixer {
    sources: Vec<Source>,
    /// Frames already produced.
    pub position: u64,
    pub warnings: Vec<String>,
}

pub struct MixerConfig {
    pub main: Option<(SourceKind, f32)>,
    pub mic: Option<(Option<String>, f32)>,
}

impl Mixer {
    pub fn start(config: MixerConfig) -> Self {
        let mut sources = Vec::new();
        let mut warnings = Vec::new();
        if let Some((kind, gain)) = config.main {
            match Source::start(kind, None, gain) {
                Ok(s) => sources.push(s),
                Err(e) => warnings.push(format!("Recording without game sound: {e}")),
            }
        }
        if let Some((device, gain)) = config.mic {
            match Source::start(SourceKind::Mic, device, gain) {
                Ok(s) => sources.push(s),
                Err(e) => warnings.push(format!("Recording without the microphone: {e}")),
            }
        }
        Self {
            sources,
            position: 0,
            warnings,
        }
    }

    pub fn has_sound(&self) -> bool {
        !self.sources.is_empty()
    }

    /// Discards what was captured before the session's first frame.
    pub fn clear(&mut self) {
        for s in &self.sources {
            s.queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    /// Mixes up to timeline frame `until` and returns the PCM bytes for
    /// `[old position, until)`. Sources that are short are padded with
    /// silence (loopback is silent by sending nothing); sources that are
    /// too far ahead lose their oldest sound.
    pub fn take_until(&mut self, until: u64) -> Vec<u8> {
        if until <= self.position {
            return Vec::new();
        }
        let n = (until - self.position) as usize;
        let mut mix = vec![[0f32; 2]; n];
        for source in &self.sources {
            let mut q = source.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.len() > n + MAX_BACKLOG {
                let excess = q.len() - n - MAX_BACKLOG / 2;
                q.drain(..excess);
            }
            let take = q.len().min(n);
            // Short sources are padded at the start: the gap was silence.
            let offset = n - take;
            for (slot, frame) in mix[offset..].iter_mut().zip(q.drain(..take)) {
                slot[0] += f32::from(frame[0]) * source.gain;
                slot[1] += f32::from(frame[1]) * source.gain;
            }
        }
        self.position = until;
        let mut out = Vec::with_capacity(n * 4);
        for [l, r] in mix {
            out.extend_from_slice(&(l.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
            out.extend_from_slice(&(r.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
        out
    }
}

pub fn ticks_to_frames(ticks: i64) -> u64 {
    (ticks.max(0) as u128 * u128::from(SAMPLE_RATE) / TICKS_PER_SEC as u128) as u64
}

pub fn frames_to_ticks(frames: u64) -> i64 {
    (u128::from(frames) * TICKS_PER_SEC as u128 / u128::from(SAMPLE_RATE)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer_with(queues: Vec<(Vec<[i16; 2]>, f32)>) -> Mixer {
        let sources = queues
            .into_iter()
            .map(|(frames, gain)| Source {
                queue: Arc::new(Mutex::new(frames.into_iter().collect())),
                gain,
                stop: Arc::new(AtomicBool::new(true)),
                thread: None,
            })
            .collect();
        Mixer {
            sources,
            position: 0,
            warnings: Vec::new(),
        }
    }

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn mixes_with_gain_and_clamps() {
        let mut m = mixer_with(vec![
            (vec![[1000, -1000]; 4], 1.0),
            (vec![[30000, -30000]; 4], 0.5),
        ]);
        let out = samples(&m.take_until(4));
        assert_eq!(&out[..2], &[16000, -16000]);
        let mut loud = mixer_with(vec![(vec![[30000, -30000]; 2], 2.0)]);
        assert_eq!(&samples(&loud.take_until(2))[..2], &[32767, -32768]);
    }

    #[test]
    fn short_sources_are_padded_with_leading_silence() {
        let mut m = mixer_with(vec![(vec![[5, 5]; 2], 1.0)]);
        assert_eq!(samples(&m.take_until(4)), vec![0, 0, 0, 0, 5, 5, 5, 5]);
        assert_eq!(m.position, 4);
        assert!(m.take_until(4).is_empty());
    }

    #[test]
    fn far_ahead_sources_drop_oldest_sound() {
        let mut frames = vec![[1i16, 1]; MAX_BACKLOG + 100];
        frames.extend(vec![[2i16, 2]; 10]);
        let mut m = mixer_with(vec![(frames, 1.0)]);
        let out = samples(&m.take_until(10));
        assert_eq!(out.len(), 20);
        let left = m.sources[0].queue.lock().unwrap().len();
        assert!(left <= MAX_BACKLOG, "{left}");
    }

    #[test]
    fn tick_frame_conversion_round_trips() {
        assert_eq!(ticks_to_frames(TICKS_PER_SEC), 48_000);
        assert_eq!(frames_to_ticks(48_000), TICKS_PER_SEC);
        assert_eq!(ticks_to_frames(-5), 0);
    }
}
