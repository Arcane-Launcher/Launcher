//! The capture engine for one game window: Windows Graphics Capture frames
//! → client-area crop + scale (GPU) → hardware H.264, plus mixed sound.
//! Feeds a live recording and/or the instant-replay buffer — short segment
//! files on disk, rotated every few seconds and joined without re-encoding
//! when a replay is saved.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{factory, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

use super::audio::{self, Mixer, MixerConfig};
use super::gpu::{Gpu, Scaler};
use super::remux;
use super::sink::{Sink, VideoFormat, TICKS_PER_SEC};

/// Replay segment length. Shorter means less to re-join and less disk
/// churn per save; longer means fewer files.
const SEGMENT_TICKS: i64 = 5 * TICKS_PER_SEC;

/// `root` and every process it started, directly or not. The launched java
/// can be a stub (Oracle's `Common Files\Oracle\Java\javapath\java.exe`)
/// whose child is the real game and owns the window.
fn process_tree(root: u32) -> HashSet<u32> {
    use sysinfo::{ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut tree = HashSet::from([root]);
    let mut grew = true;
    while grew {
        grew = false;
        for (pid, process) in sys.processes() {
            let Some(parent) = process.parent() else {
                continue;
            };
            // A parent id can be reused by an unrelated, newer process.
            let parent_is_older = sys
                .process(parent)
                .is_none_or(|p| p.start_time() <= process.start_time());
            if tree.contains(&parent.as_u32()) && parent_is_older && tree.insert(pid.as_u32()) {
                grew = true;
            }
        }
    }
    tree
}

/// The game's main window: the largest visible, non-tool top-level window
/// of the launched process or its children.
pub fn find_game_window(pid: u32) -> Option<isize> {
    struct Search {
        pids: HashSet<u32>,
        best: Option<(isize, i64)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> windows::core::BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if search.pids.contains(&pid) && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            let mut rect = RECT::default();
            if ex_style & WS_EX_TOOLWINDOW.0 == 0
                && unsafe { GetClientRect(hwnd, &mut rect) }.is_ok()
            {
                let area = i64::from(rect.right) * i64::from(rect.bottom);
                if area > 0 && search.best.is_none_or(|(_, a)| area > a) {
                    search.best = Some((hwnd.0 as isize, area));
                }
            }
        }
        true.into()
    }
    let mut search = Search {
        pids: process_tree(pid),
        best: None,
    };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.best.map(|(h, _)| h)
}

/// The window's client area (no title bar or borders) inside the captured
/// frame, which covers the window's visible bounds.
fn client_crop(hwnd: HWND, content: SizeInt32) -> Option<RECT> {
    unsafe {
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client).ok()?;
        let mut origin = POINT::default();
        if !ClientToScreen(hwnd, &mut origin).as_bool() {
            return None;
        }
        let mut bounds = RECT::default();
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut bounds as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
        let left = (origin.x - bounds.left).clamp(0, content.Width);
        let top = (origin.y - bounds.top).clamp(0, content.Height);
        let right = (left + client.right).min(content.Width);
        let bottom = (top + client.bottom).min(content.Height);
        (right - left >= 16 && bottom - top >= 16).then_some(RECT {
            left,
            top,
            right,
            bottom,
        })
    }
}

/// One output file fed from the session timeline. Its own time 0 is the
/// session time of its first video frame (`start`).
struct Track {
    sink: Sink,
    start: i64,
    /// Session time after which nothing more is written.
    end: Option<i64>,
}

impl Track {
    fn write_audio(&mut self, pcm: &[u8], from_frame: u64) {
        if pcm.is_empty() {
            return;
        }
        let start_frame = audio::ticks_to_frames(self.start);
        let end_frame = self.end.map(audio::ticks_to_frames).unwrap_or(u64::MAX);
        let count = (pcm.len() / 4) as u64;
        let a = from_frame.max(start_frame);
        let b = (from_frame + count).min(end_frame);
        if a >= b {
            return;
        }
        let slice = &pcm[((a - from_frame) * 4) as usize..((b - from_frame) * 4) as usize];
        if let Err(e) = self
            .sink
            .write_audio(slice, audio::frames_to_ticks(a - start_frame))
        {
            log::error!(
                "recorder: writing audio to {}: {e}",
                self.sink.path.display()
            );
        }
    }
}

type Done = Arc<Mutex<Option<bool>>>;

struct Segment {
    path: PathBuf,
    start: i64,
    end: i64,
    /// Set once the file is finalized: `true` if it's usable.
    done: Done,
}

struct Replay {
    dir: PathBuf,
    keep: i64,
    current: Option<Track>,
    segments: VecDeque<Segment>,
    next_index: u64,
    /// The next segment's writer, created ahead of time off the capture
    /// thread so a rotation doesn't stall a frame.
    spare: Arc<Mutex<Option<Sink>>>,
}

impl Replay {
    fn segment_path(&mut self) -> PathBuf {
        self.next_index += 1;
        self.dir.join(format!("segment-{:06}.mp4", self.next_index))
    }
}

struct Pipeline {
    gpu: Gpu,
    scaler: Scaler,
    mixer: Mixer,
    format: VideoFormat,
    hwnd: isize,
    pool_size: SizeInt32,
    t0: Option<i64>,
    last_sent: Option<i64>,
    pending_recording: Option<PathBuf>,
    recording: Option<Track>,
    replay: Option<Replay>,
    /// Rotated replay segments still taking in their last bit of sound.
    closing: Vec<(Track, Done)>,
    error: Option<String>,
    /// Frames delivered by the capture / encoded, and time spent on them.
    stats: (u64, u64, Duration),
}

// Every COM object in here is used only under the pipeline's mutex, and the
// device is multithread-protected.
unsafe impl Send for Pipeline {}

struct SendManager(IMFDXGIDeviceManager);
unsafe impl Send for SendManager {}

impl Pipeline {
    fn with_audio(&self) -> bool {
        self.mixer.has_sound()
    }

    fn new_sink(&self, path: &Path) -> windows::core::Result<Sink> {
        Sink::create(&self.gpu.manager, path, &self.format, self.with_audio())
    }

    fn prepare_spare(&mut self) {
        let Some(replay) = self.replay.as_mut() else {
            return;
        };
        let path = replay.segment_path();
        let spare = replay.spare.clone();
        let manager = SendManager(self.gpu.manager.clone());
        let format = self.format;
        let with_audio = self.mixer.has_sound();
        std::thread::spawn(move || {
            let manager = manager;
            match Sink::create(&manager.0, &path, &format, with_audio) {
                Ok(sink) => *spare.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink),
                Err(e) => log::error!("recorder: preparing replay segment: {e}"),
            }
        });
    }

    fn on_frame(
        &mut self,
        pool: &Direct3D11CaptureFramePool,
        frame: Direct3D11CaptureFrame,
    ) -> windows::core::Result<()> {
        let began = Instant::now();
        self.stats.0 += 1;
        let size = frame.ContentSize()?;
        if size.Width != self.pool_size.Width || size.Height != self.pool_size.Height {
            drop(frame);
            pool.Recreate(
                &self.gpu.winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            )?;
            self.pool_size = size;
            return Ok(());
        }
        let ts = frame.SystemRelativeTime()?.Duration;
        if self.t0.is_none() {
            self.t0 = Some(ts);
            self.mixer.clear();
        }
        let now = ts - self.t0.expect("set above");
        let interval = TICKS_PER_SEC / i64::from(self.format.fps);
        if self
            .last_sent
            .is_some_and(|last| now - last < interval * 3 / 4)
        {
            return Ok(());
        }
        let hwnd = HWND(self.hwnd as *mut _);
        let Some(crop) = client_crop(hwnd, size) else {
            return Ok(());
        };
        let texture: ID3D11Texture2D = unsafe {
            frame
                .Surface()?
                .cast::<IDirect3DDxgiInterfaceAccess>()?
                .GetInterface()?
        };
        let output = self.scaler.process(&self.gpu, &texture, crop)?;
        drop(texture);
        drop(frame);

        if let Some(path) = self.pending_recording.take() {
            self.recording = Some(Track {
                sink: self.new_sink(&path)?,
                start: now,
                end: None,
            });
        }
        if let Some(track) = self.recording.as_mut() {
            track.sink.write_video(&output, now - track.start)?;
        }

        if self.replay.is_some() {
            let rotate = self
                .replay
                .as_ref()
                .and_then(|r| r.current.as_ref())
                .is_some_and(|c| now - c.start >= SEGMENT_TICKS);
            if rotate {
                self.rotate_segment(now);
            }
            if self.replay.as_ref().is_some_and(|r| r.current.is_none()) {
                let spare = self
                    .replay
                    .as_ref()
                    .and_then(|r| r.spare.lock().unwrap_or_else(|e| e.into_inner()).take());
                let sink = match spare {
                    Some(sink) => sink,
                    None => {
                        let path = self.replay.as_mut().expect("checked").segment_path();
                        self.new_sink(&path)?
                    }
                };
                self.replay.as_mut().expect("checked").current = Some(Track {
                    sink,
                    start: now,
                    end: None,
                });
                self.prepare_spare();
            }
            if let Some(track) = self.replay.as_mut().and_then(|r| r.current.as_mut()) {
                track.sink.write_video(&output, now - track.start)?;
            }
        }
        self.last_sent = Some(now);
        self.stats.1 += 1;

        let until = audio::ticks_to_frames(now).saturating_sub(audio::LATENCY_FRAMES);
        self.pump_audio(until);
        self.finish_closing();
        self.prune_segments(now);
        self.stats.2 += began.elapsed();
        Ok(())
    }

    fn pump_audio(&mut self, until: u64) {
        if !self.with_audio() {
            return;
        }
        let from = self.mixer.position;
        let pcm = self.mixer.take_until(until);
        if pcm.is_empty() {
            return;
        }
        if let Some(t) = self.recording.as_mut() {
            t.write_audio(&pcm, from);
        }
        if let Some(t) = self.replay.as_mut().and_then(|r| r.current.as_mut()) {
            t.write_audio(&pcm, from);
        }
        for (t, _) in &mut self.closing {
            t.write_audio(&pcm, from);
        }
    }

    fn frame_end(&self) -> i64 {
        self.last_sent.unwrap_or(0) + TICKS_PER_SEC / i64::from(self.format.fps)
    }

    /// Ends the current replay segment at `at`; it's finalized once its
    /// sound has caught up.
    fn rotate_segment(&mut self, at: i64) {
        let Some(replay) = self.replay.as_mut() else {
            return;
        };
        let Some(mut track) = replay.current.take() else {
            return;
        };
        track.end = Some(at);
        let done: Done = Arc::new(Mutex::new(None));
        replay.segments.push_back(Segment {
            path: track.sink.path.clone(),
            start: track.start,
            end: at,
            done: done.clone(),
        });
        self.closing.push((track, done));
    }

    fn finish_closing(&mut self) {
        let position = self.mixer.position;
        let with_audio = self.with_audio();
        let mut i = 0;
        while i < self.closing.len() {
            let caught_up = !with_audio
                || self.closing[i]
                    .0
                    .end
                    .is_some_and(|end| position >= audio::ticks_to_frames(end));
            if caught_up {
                let (track, done) = self.closing.swap_remove(i);
                std::thread::spawn(move || {
                    let ok = matches!(track.sink.finish(), Ok(Some(_)));
                    *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(ok);
                });
            } else {
                i += 1;
            }
        }
    }

    fn prune_segments(&mut self, now: i64) {
        let Some(replay) = self.replay.as_mut() else {
            return;
        };
        while let Some(first) = replay.segments.front() {
            let finished = first
                .done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if finished && first.end < now - replay.keep - SEGMENT_TICKS {
                let _ = std::fs::remove_file(&first.path);
                replay.segments.pop_front();
            } else {
                break;
            }
        }
    }
}

pub struct EngineConfig {
    pub hwnd: isize,
    pub format: VideoFormat,
    pub audio: MixerConfig,
    pub replay: Option<(PathBuf, u32)>,
}

pub struct Engine {
    pipeline: Arc<Mutex<Pipeline>>,
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    closed: Arc<AtomicBool>,
    pub warnings: Vec<String>,
}

unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Engine {
    pub fn start(config: EngineConfig) -> Result<Self, String> {
        let gpu = Gpu::new()
            .map_err(|e| format!("Couldn't set up the graphics card for recording: {e}"))?;
        let scaler = Scaler::new(
            &gpu,
            config.format.width,
            config.format.height,
            config.format.fps,
        )
        .map_err(|e| format!("Couldn't set up video scaling: {e}"))?;
        let hwnd = HWND(config.hwnd as *mut _);
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err("Minecraft's window closed.".into());
        }
        let item: GraphicsCaptureItem = unsafe {
            factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .and_then(|interop| interop.CreateForWindow(hwnd))
                .map_err(|e| format!("Couldn't capture Minecraft's window: {e}"))?
        };
        let size = item.Size().map_err(err)?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &gpu.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(err)?;
        let session = pool.CreateCaptureSession(&item).map_err(err)?;
        // Windows 11: no yellow capture border around the game.
        let _ = session.SetIsBorderRequired(false);

        let replay = match config.replay {
            Some((dir, seconds)) => {
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(err)?;
                Some(Replay {
                    dir,
                    keep: i64::from(seconds) * TICKS_PER_SEC,
                    current: None,
                    segments: VecDeque::new(),
                    next_index: 0,
                    spare: Arc::new(Mutex::new(None)),
                })
            }
            None => None,
        };
        let mixer = Mixer::start(config.audio);
        let warnings = mixer.warnings.clone();
        let pipeline = Arc::new(Mutex::new(Pipeline {
            gpu,
            scaler,
            mixer,
            format: config.format,
            hwnd: config.hwnd,
            pool_size: size,
            t0: None,
            last_sent: None,
            pending_recording: None,
            recording: None,
            replay,
            closing: Vec::new(),
            error: None,
            stats: (0, 0, Duration::ZERO),
        }));
        lock(&pipeline).prepare_spare();

        let frame_pipeline = pipeline.clone();
        pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            windows::core::IInspectable,
        >::new(move |sender, _| {
            let Some(pool) = sender.as_ref() else {
                return Ok(());
            };
            let frame = pool.TryGetNextFrame()?;
            let mut p = lock(&frame_pipeline);
            if p.error.is_none() {
                if let Err(e) = p.on_frame(pool, frame) {
                    log::error!("recorder: frame failed: {e}");
                    p.error = Some(e.message().to_string());
                }
            }
            Ok(())
        }))
        .map_err(err)?;
        let closed = Arc::new(AtomicBool::new(false));
        let closed_flag = closed.clone();
        item.Closed(&TypedEventHandler::<
            GraphicsCaptureItem,
            windows::core::IInspectable,
        >::new(move |_, _| {
            closed_flag.store(true, Ordering::Relaxed);
            Ok(())
        }))
        .map_err(err)?;
        session
            .StartCapture()
            .map_err(|e| format!("Couldn't start capturing: {e}"))?;
        Ok(Self {
            pipeline,
            session,
            pool,
            closed,
            warnings,
        })
    }

    pub fn is_closed(&self) -> bool {
        let hwnd = HWND(lock(&self.pipeline).hwnd as *mut _);
        self.closed.load(Ordering::Relaxed) || !unsafe { IsWindow(Some(hwnd)) }.as_bool()
    }

    /// (frames captured, frames encoded, average time per frame).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stats(&self) -> (u64, u64, Duration) {
        let (seen, sent, total) = lock(&self.pipeline).stats;
        (seen, sent, total / (seen.max(1) as u32))
    }

    pub fn take_error(&self) -> Option<String> {
        lock(&self.pipeline).error.clone()
    }

    pub fn is_recording(&self) -> bool {
        let p = lock(&self.pipeline);
        p.recording.is_some() || p.pending_recording.is_some()
    }

    pub fn replay_active(&self) -> bool {
        lock(&self.pipeline).replay.is_some()
    }

    pub fn start_recording(&self, path: PathBuf) {
        let mut p = lock(&self.pipeline);
        if p.recording.is_none() {
            p.pending_recording = Some(path);
        }
    }

    /// Ends the recording and returns its file (`None` if not a single
    /// frame was captured).
    pub fn stop_recording(&self) -> Result<Option<PathBuf>, String> {
        let track = {
            let mut p = lock(&self.pipeline);
            p.pending_recording = None;
            let Some(mut track) = p.recording.take() else {
                return Ok(None);
            };
            let end = p.frame_end();
            track.end = Some(end);
            // Write its sound right up to the end now rather than waiting
            // for more frames (there may be none, e.g. minimized).
            let until = audio::ticks_to_frames(end);
            if p.with_audio() {
                let from = p.mixer.position;
                let pcm = p.mixer.take_until(until);
                track.write_audio(&pcm, from);
                if let Some(t) = p.replay.as_mut().and_then(|r| r.current.as_mut()) {
                    t.write_audio(&pcm, from);
                }
                for (t, _) in &mut p.closing {
                    t.write_audio(&pcm, from);
                }
            }
            track
        };
        track
            .sink
            .finish()
            .map_err(|e| format!("Couldn't finish the video file: {e}"))
    }

    /// Saves the last `keep` seconds of the replay buffer to `out`.
    pub fn save_replay(&self, out: &Path) -> Result<PathBuf, String> {
        let (segments, keep, end) = {
            let mut p = lock(&self.pipeline);
            if p.replay.is_none() {
                return Err("Instant replay isn't running.".into());
            }
            let end = p.frame_end();
            if p.replay.as_ref().and_then(|r| r.current.as_ref()).is_some() {
                p.rotate_segment(end);
                if p.with_audio() {
                    let until = audio::ticks_to_frames(end);
                    let from = p.mixer.position;
                    let pcm = p.mixer.take_until(until);
                    if let Some(t) = p.recording.as_mut() {
                        t.write_audio(&pcm, from);
                    }
                    for (t, _) in &mut p.closing {
                        t.write_audio(&pcm, from);
                    }
                }
                p.finish_closing();
            }
            let replay = p.replay.as_ref().expect("checked above");
            let keep = replay.keep;
            let segments: Vec<(PathBuf, i64, i64, Done)> = replay
                .segments
                .iter()
                .filter(|s| s.end > end - keep)
                .map(|s| (s.path.clone(), s.start, s.end, s.done.clone()))
                .collect();
            (segments, keep, end)
        };
        if segments.is_empty() {
            return Err("Nothing has been buffered yet. Play for a few seconds first.".into());
        }
        // Wait for the segments to be finalized.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut inputs = Vec::new();
        for (path, start, _, done) in &segments {
            loop {
                let state = *done.lock().unwrap_or_else(|e| e.into_inner());
                match state {
                    Some(true) => {
                        inputs.push(remux::Input {
                            path: path.clone(),
                            offset: *start,
                        });
                        break;
                    }
                    Some(false) => break,
                    None if Instant::now() > deadline => return Err("Saving took too long.".into()),
                    None => std::thread::sleep(Duration::from_millis(30)),
                }
            }
        }
        remux::join(&inputs, (end - keep).max(0), None, out)
            .map_err(|e| format!("Couldn't save the replay: {e}"))?;
        Ok(out.to_path_buf())
    }

    /// Stops capturing. Returns the in-progress recording's result, if one
    /// was running; the replay buffer is discarded.
    pub fn shutdown(&self) -> Option<Result<Option<PathBuf>, String>> {
        let _ = self.session.Close();
        let _ = self.pool.Close();
        let recording = self.is_recording().then(|| self.stop_recording());
        let mut p = lock(&self.pipeline);
        for (track, _) in p.closing.drain(..) {
            let _ = track.sink.finish();
        }
        if let Some(replay) = p.replay.take() {
            if let Some(track) = replay.current {
                drop(track);
            }
            drop(
                replay
                    .spare
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
            );
            let _ = std::fs::remove_dir_all(&replay.dir);
        }
        recording
    }
}

fn lock(p: &Mutex<Pipeline>) -> MutexGuard<'_, Pipeline> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::recorder::win::audio::SourceKind;

    /// Live end-to-end check against a real window that plays sound and
    /// resizes itself (RECORDER_TEST_PID, RECORDER_TEST_DIR): records,
    /// saves an instant replay, then trims and compresses the recording.
    /// Run with `--ignored`.
    #[test]
    #[ignore]
    fn live_record_replay_trim_compress() {
        let pid: u32 = std::env::var("RECORDER_TEST_PID").unwrap().parse().unwrap();
        let dir = PathBuf::from(std::env::var("RECORDER_TEST_DIR").unwrap());
        let hwnd = (0..50)
            .find_map(|_| {
                find_game_window(pid).or_else(|| {
                    std::thread::sleep(Duration::from_millis(200));
                    None
                })
            })
            .expect("no window for that pid");
        let engine = Engine::start(EngineConfig {
            hwnd,
            format: VideoFormat {
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate: 12_000_000,
            },
            audio: MixerConfig {
                main: Some((SourceKind::Game(pid), 1.0)),
                mic: None,
            },
            replay: std::env::var("RECORDER_TEST_NO_REPLAY")
                .is_err()
                .then(|| (dir.join("replay-cache"), 6)),
        })
        .unwrap();
        assert!(engine.warnings.is_empty(), "{:?}", engine.warnings);
        std::thread::sleep(Duration::from_secs(2));
        engine.start_recording(dir.join("recording.mp4"));
        std::thread::sleep(Duration::from_secs(8));
        assert_eq!(engine.take_error(), None);
        println!("stats (captured, encoded, avg time): {:?}", engine.stats());
        if !engine.replay_active() {
            let recording = engine.stop_recording().unwrap().expect("no recording file");
            println!(
                "recording {:.2}s",
                remux::duration(&recording).unwrap() as f64 / 1e7
            );
            return;
        }
        let replay = engine.save_replay(&dir.join("replay.mp4")).unwrap();
        println!("replay: {}", replay.display());
        std::thread::sleep(Duration::from_secs(1));
        let recording = engine.stop_recording().unwrap().expect("no recording file");
        println!("recording: {}", recording.display());
        assert!(engine.shutdown().is_none());

        let rec_secs = remux::duration(&recording).unwrap() as f64 / 1e7;
        let replay_secs = remux::duration(&replay).unwrap() as f64 / 1e7;
        println!("recording {rec_secs:.2}s, replay {replay_secs:.2}s");
        assert!((7.0..10.5).contains(&rec_secs), "{rec_secs}");
        assert!((4.5..7.5).contains(&replay_secs), "{replay_secs}");

        let trimmed = dir.join("trimmed.mp4");
        let ticks = |s: f64| (s * 1e7) as i64;
        remux::join(
            &[remux::Input {
                path: recording.clone(),
                offset: 0,
            }],
            ticks(2.0),
            Some(ticks(5.0)),
            &trimmed,
        )
        .unwrap();
        let trim_secs = remux::duration(&trimmed).unwrap() as f64 / 1e7;
        println!("trimmed {trim_secs:.2}s");
        assert!((2.0..3.3).contains(&trim_secs), "{trim_secs}");

        let small = dir.join("small.mp4");
        remux::compress(&recording, 0, ticks(rec_secs), 2 * 1024 * 1024, 720, &small).unwrap();
        let size = std::fs::metadata(&small).unwrap().len();
        println!("compressed to {size} bytes");
        assert!(size <= 2 * 1024 * 1024, "{size}");

        if std::env::var("RECORDER_TEST_CLIPBOARD").is_ok() {
            remux::copy_file_to_clipboard(&small).unwrap();
        }
    }
}
