//! Post-processing on finished .mp4 files with Media Foundation:
//! - `join`: copies the encoded streams of one or more files into a new one
//!   without re-encoding (instant replay segments, lossless trims). Cuts
//!   start on a keyframe — one per second in our recordings.
//! - `compress`: re-encodes a range at a bitrate that fits a size limit.

use std::path::{Path, PathBuf};

use windows::core::{Result, HSTRING};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::gpu::mf_startup;
use super::sink::{self, VideoFormat, TICKS_PER_SEC};

pub fn init_thread() {
    mf_startup();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// A file and where its time 0 sits on the output's source timeline.
pub struct Input {
    pub path: PathBuf,
    pub offset: i64,
}

fn reader(path: &Path, attrs: Option<&IMFAttributes>) -> Result<IMFSourceReader> {
    unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), attrs) }
}

/// (stream index, is_video) for every audio/video stream in the file.
fn streams(reader: &IMFSourceReader) -> Vec<(u32, bool)> {
    let mut out = Vec::new();
    for i in 0..8u32 {
        let Ok(t) = (unsafe { reader.GetNativeMediaType(i, 0) }) else {
            break;
        };
        match unsafe { t.GetGUID(&MF_MT_MAJOR_TYPE) } {
            Ok(major) if major == MFMediaType_Video => out.push((i, true)),
            Ok(major) if major == MFMediaType_Audio => out.push((i, false)),
            _ => {}
        }
    }
    out
}

/// Duration of an .mp4 in 100 ns ticks.
#[cfg_attr(not(test), allow(dead_code))]
pub fn duration(path: &Path) -> Result<i64> {
    let reader = reader(path, None)?;
    let value = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)?
    };
    Ok(i64::try_from(u64::try_from(&value).unwrap_or(0)).unwrap_or(0))
}

struct Read {
    stream: u32,
    flags: u32,
    time: i64,
    sample: Option<IMFSample>,
}

fn read(reader: &IMFSourceReader) -> Result<Read> {
    let (mut stream, mut flags, mut time, mut sample) = (0u32, 0u32, 0i64, None);
    unsafe {
        reader.ReadSample(
            MF_SOURCE_READER_ANY_STREAM.0 as u32,
            0,
            Some(&mut stream),
            Some(&mut flags),
            Some(&mut time),
            Some(&mut sample),
        )?;
    }
    Ok(Read {
        stream,
        flags,
        time,
        sample,
    })
}

fn is_keyframe(sample: &IMFSample) -> bool {
    unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0
}

/// Copies `[from, to)` of the inputs' source timeline into `out`, starting
/// at the first keyframe at or after `from`. Returns the output's duration.
pub fn join(inputs: &[Input], from: i64, to: Option<i64>, out: &Path) -> Result<i64> {
    init_thread();
    let mut writer: Option<(IMFSinkWriter, Vec<(bool, u32)>)> = None;
    let mut base: Option<i64> = None;
    let mut last_end = 0i64;
    for input in inputs {
        let reader = reader(&input.path, None)?;
        let found = streams(&reader);
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            for (i, _) in &found {
                reader.SetStreamSelection(*i, true)?;
            }
        }
        if writer.is_none() {
            let w = sink::create_writer(out, None)?;
            let mut map = Vec::new();
            for (i, is_video) in &found {
                let native = unsafe { reader.GetNativeMediaType(*i, 0)? };
                let index = unsafe { w.AddStream(&native)? };
                unsafe { w.SetInputMediaType(index, &native, None)? };
                map.push((*is_video, index));
            }
            unsafe { w.BeginWriting()? };
            writer = Some((w, map));
        }
        let (w, map) = writer.as_ref().expect("created above");
        loop {
            let r = read(&reader)?;
            if r.flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 && r.sample.is_none() {
                break;
            }
            let Some(sample) = r.sample else { continue };
            let Some(pos) = found.iter().position(|(i, _)| *i == r.stream) else {
                continue;
            };
            let (is_video, out_index) = map.get(pos).copied().unwrap_or((false, u32::MAX));
            if out_index == u32::MAX {
                continue;
            }
            let t = input.offset + r.time;
            if to.is_some_and(|to| t >= to) {
                continue;
            }
            if base.is_none() {
                if is_video && t >= from && is_keyframe(&sample) {
                    base = Some(t);
                } else {
                    continue;
                }
            }
            let b = base.expect("set above");
            if t < b {
                continue;
            }
            unsafe {
                let duration = sample.GetSampleDuration().unwrap_or(0);
                sample.SetSampleTime(t - b)?;
                w.WriteSample(out_index, &sample)?;
                last_end = last_end.max(t - b + duration);
            }
        }
    }
    let Some((w, _)) = writer else {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "nothing to join",
        ));
    };
    if base.is_none() {
        drop(w);
        let _ = std::fs::remove_file(out);
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "no keyframe in range",
        ));
    }
    unsafe { w.Finalize()? };
    Ok(last_end)
}

/// Re-encodes `[from, to)` of `input` into `out` so the whole file comes
/// out at about `target_bytes`, scaled to at most `max_height`.
pub fn compress(
    input: &Path,
    from: i64,
    to: i64,
    target_bytes: u64,
    max_height: u32,
    out: &Path,
) -> std::result::Result<(), String> {
    init_thread();
    let length = (to - from).max(TICKS_PER_SEC / 10);
    let seconds = length as f64 / TICKS_PER_SEC as f64;
    const AUDIO_BITRATE: u32 = 128_000;
    // Leave ~6% for the container and encoder overshoot.
    let total_bits = target_bytes as f64 * 8.0 * 0.94;
    let video_bitrate = (total_bits / seconds) - f64::from(AUDIO_BITRATE);
    if video_bitrate < 250_000.0 {
        let max_seconds = (total_bits / (250_000.0 + f64::from(AUDIO_BITRATE))).floor();
        return Err(format!(
            "This clip is too long to fit. Trim it to {max_seconds:.0} seconds or less first."
        ));
    }
    let mut bitrate = video_bitrate.min(8_000_000.0) as u32;
    // Encoders can overshoot; one more pass at a scaled-down bitrate if so.
    for attempt in 0..2 {
        run_compress(input, from, to, bitrate, AUDIO_BITRATE, max_height, out)
            .map_err(|e| e.to_string())?;
        let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
        if size <= target_bytes || attempt == 1 {
            return if size <= target_bytes {
                Ok(())
            } else {
                let _ = std::fs::remove_file(out);
                Err("Couldn't get this clip small enough. Trim it shorter and try again.".into())
            };
        }
        bitrate = ((f64::from(bitrate) * target_bytes as f64 / size as f64) * 0.9) as u32;
        if bitrate < 200_000 {
            let _ = std::fs::remove_file(out);
            return Err("This clip is too long to fit. Trim it shorter first.".into());
        }
    }
    unreachable!()
}

fn run_compress(
    input: &Path,
    from: i64,
    to: i64,
    video_bitrate: u32,
    audio_bitrate: u32,
    max_height: u32,
    out: &Path,
) -> Result<()> {
    let attrs = sink::attributes(2)?;
    unsafe { attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)? };
    let reader = reader(input, Some(&attrs))?;
    let found = streams(&reader);
    let video_stream = found.iter().find(|(_, v)| *v).map(|(i, _)| *i);
    let audio_stream = found.iter().find(|(_, v)| !*v).map(|(i, _)| *i);
    let Some(video_stream) = video_stream else {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "no video stream",
        ));
    };

    let native = unsafe { reader.GetNativeMediaType(video_stream, 0)? };
    let size = unsafe { native.GetUINT64(&MF_MT_FRAME_SIZE)? };
    let (src_w, src_h) = ((size >> 32) as u32, size as u32);
    let rate = unsafe { native.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(sink::pack(60, 1));
    let fps = ((rate >> 32) as u32 / (rate as u32).max(1)).clamp(1, 120);
    let height = src_h.min(max_height) & !1;
    let width = ((u64::from(src_w) * u64::from(height) / u64::from(src_h.max(1))) as u32) & !1;
    let format = VideoFormat {
        width,
        height,
        fps,
        bitrate: video_bitrate,
    };

    unsafe {
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(video_stream, true)?;
        reader.SetCurrentMediaType(video_stream, None, &sink::nv12_type(&format)?)?;
        if let Some(a) = audio_stream {
            reader.SetStreamSelection(a, true)?;
            reader.SetCurrentMediaType(a, None, &sink::pcm_type()?)?;
        }
    }

    let writer = sink::create_writer(out, None)?;
    let (out_video, out_audio) = unsafe {
        let v = writer.AddStream(&sink::h264_type(&format)?)?;
        writer.SetInputMediaType(
            v,
            &sink::nv12_type(&format)?,
            &sink::constant_bitrate_params(fps, video_bitrate)?,
        )?;
        let a = match audio_stream {
            Some(_) => {
                let a = writer.AddStream(&sink::aac_type(audio_bitrate)?)?;
                writer.SetInputMediaType(a, &sink::pcm_type()?, None)?;
                Some(a)
            }
            None => None,
        };
        writer.BeginWriting()?;
        (v, a)
    };

    let mut wrote_video = false;
    loop {
        let r = read(&reader)?;
        if r.flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 && r.sample.is_none() {
            break;
        }
        let Some(sample) = r.sample else { continue };
        if r.time < from || r.time >= to {
            if r.time >= to && r.stream == video_stream {
                break;
            }
            continue;
        }
        let target = if r.stream == video_stream {
            wrote_video = true;
            out_video
        } else if Some(r.stream) == audio_stream {
            match out_audio {
                Some(a) => a,
                None => continue,
            }
        } else {
            continue;
        };
        unsafe {
            sample.SetSampleTime(r.time - from)?;
            writer.WriteSample(target, &sample)?;
        }
    }
    if !wrote_video {
        drop(writer);
        let _ = std::fs::remove_file(out);
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "no video in range",
        ));
    }
    unsafe { writer.Finalize()? };
    Ok(())
}

/// Puts `path` on the clipboard as a file, so Ctrl+V in Discord (or
/// Explorer) attaches it.
pub fn copy_file_to_clipboard(path: &Path) -> std::result::Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
    };
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;

    let wide: Vec<u16> = path.as_os_str().encode_wide_with_nulls();
    let header = std::mem::size_of::<DROPFILES>();
    let bytes = header + wide.len() * 2;
    unsafe {
        let memory: HGLOBAL =
            GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes).map_err(|e| e.to_string())?;
        let ptr = GlobalLock(memory) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("couldn't lock clipboard memory".into());
        }
        let drop_files = DROPFILES {
            pFiles: header as u32,
            pt: POINT::default(),
            fNC: false.into(),
            fWide: true.into(),
        };
        std::ptr::write_unaligned(ptr as *mut DROPFILES, drop_files);
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
        let _ = GlobalUnlock(memory);

        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !opened {
            let _ = GlobalFree(Some(memory));
            return Err("the clipboard is busy".into());
        }
        let result = EmptyClipboard()
            .and_then(|_| SetClipboardData(u32::from(CF_HDROP.0), Some(HANDLE(memory.0))));
        let _ = CloseClipboard();
        if let Err(e) = result {
            let _ = GlobalFree(Some(memory));
            return Err(e.to_string());
        }
    }
    Ok(())
}

trait EncodeWideWithNulls {
    /// UTF-16 path followed by the two NULs a DROPFILES list ends with.
    fn encode_wide_with_nulls(&self) -> Vec<u16>;
}

impl EncodeWideWithNulls for std::ffi::OsStr {
    fn encode_wide_with_nulls(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0, 0]).collect()
    }
}
