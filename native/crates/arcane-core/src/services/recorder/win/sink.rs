//! Writes H.264 + AAC .mp4 files with Media Foundation's sink writer, fed
//! NV12 GPU textures (encoded by the graphics card's hardware encoder) and
//! 48 kHz 16-bit stereo PCM.

use std::path::{Path, PathBuf};

use windows::core::{Interface, Result, HSTRING};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
pub const BLOCK_ALIGN: u32 = 4;
pub const TICKS_PER_SEC: i64 = 10_000_000;

#[derive(Clone, Copy, Debug)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

pub fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

pub fn attributes(capacity: u32) -> Result<IMFAttributes> {
    let mut attrs = None;
    unsafe { MFCreateAttributes(&mut attrs, capacity)? };
    Ok(attrs.expect("MFCreateAttributes returned nothing"))
}

pub fn h264_type(f: &VideoFormat) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, f.bitrate)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(f.width, f.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(f.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        Ok(t)
    }
}

pub fn nv12_type(f: &VideoFormat) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(f.width, f.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(f.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        Ok(t)
    }
}

pub fn aac_type(bitrate: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate / 8)?;
        Ok(t)
    }
}

pub fn pcm_type() -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, BLOCK_ALIGN)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE * BLOCK_ALIGN)?;
        Ok(t)
    }
}

/// Keyframe every second, so trims and replay joins land within a second
/// of where they were asked for.
pub fn encoder_params(fps: u32) -> Result<IMFAttributes> {
    let params = attributes(2)?;
    unsafe { params.SetUINT32(&CODECAPI_AVEncMPVGOPSize, fps)? };
    Ok(params)
}

/// Like [`encoder_params`], but holds the encoder to `bitrate` — for files
/// that must fit a size limit.
pub fn constant_bitrate_params(fps: u32, bitrate: u32) -> Result<IMFAttributes> {
    let params = encoder_params(fps)?;
    unsafe {
        params.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32)?;
        params.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, bitrate)?;
    }
    Ok(params)
}

pub fn create_writer(path: &Path, manager: Option<&IMFDXGIDeviceManager>) -> Result<IMFSinkWriter> {
    let attrs = attributes(3)?;
    unsafe {
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        if let Some(manager) = manager {
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, manager)?;
        }
        MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)
    }
}

pub fn pcm_sample(pcm: &[u8], time: i64) -> Result<IMFSample> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(pcm.as_ptr(), data, pcm.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(pcm.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        let frames = (pcm.len() as u32 / BLOCK_ALIGN) as i64;
        sample.SetSampleDuration(frames * TICKS_PER_SEC / i64::from(SAMPLE_RATE))?;
        Ok(sample)
    }
}

/// One output file being encoded live.
pub struct Sink {
    writer: IMFSinkWriter,
    pub path: PathBuf,
    video: u32,
    audio: Option<u32>,
    frame_duration: i64,
    pub frames: u64,
    last_video_time: i64,
}

impl Sink {
    pub fn create(manager: &IMFDXGIDeviceManager, path: &Path, format: &VideoFormat, with_audio: bool) -> Result<Self> {
        let writer = create_writer(path, Some(manager))?;
        unsafe {
            let video = writer.AddStream(&h264_type(format)?)?;
            writer.SetInputMediaType(video, &nv12_type(format)?, &encoder_params(format.fps)?)?;
            let audio = if with_audio {
                let index = writer.AddStream(&aac_type(192_000)?)?;
                writer.SetInputMediaType(index, &pcm_type()?, None)?;
                Some(index)
            } else {
                None
            };
            writer.BeginWriting()?;
            Ok(Self {
                writer,
                path: path.to_path_buf(),
                video,
                audio,
                frame_duration: TICKS_PER_SEC / i64::from(format.fps),
                frames: 0,
                last_video_time: -1,
            })
        }
    }

    pub fn write_video(&mut self, texture: &ID3D11Texture2D, time: i64) -> Result<()> {
        // Timestamps must strictly increase.
        let time = time.max(self.last_video_time + 1);
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
            let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
            buffer.SetCurrentLength(length)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(self.frame_duration)?;
            self.writer.WriteSample(self.video, &sample)?;
        }
        self.last_video_time = time;
        self.frames += 1;
        Ok(())
    }

    pub fn write_audio(&mut self, pcm: &[u8], time: i64) -> Result<()> {
        let Some(index) = self.audio else { return Ok(()) };
        if pcm.is_empty() || time < 0 {
            return Ok(());
        }
        unsafe { self.writer.WriteSample(index, &pcm_sample(pcm, time)?) }
    }

    /// Flushes the encoder and closes the file. Removes it if no video
    /// frame was ever written (it would not be playable).
    pub fn finish(self) -> Result<Option<PathBuf>> {
        if self.frames == 0 {
            drop(self.writer);
            let _ = std::fs::remove_file(&self.path);
            return Ok(None);
        }
        unsafe { self.writer.Finalize()? };
        Ok(Some(self.path))
    }
}

// The sink writer is free-threaded.
unsafe impl Send for Sink {}
