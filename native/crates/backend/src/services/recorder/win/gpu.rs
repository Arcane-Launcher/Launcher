//! The recorder's own D3D11 device (video-capable, multithread-protected so
//! Media Foundation's encoder can share it) and a video-processor scaler
//! that crops the game window to its client area and fits it into a fixed
//! NV12 output — the encoder's native input — entirely on the GPU.

use std::sync::Once;

use windows::core::{Interface, Result};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::{
    IMFDXGIDeviceManager, MFCreateDXGIDeviceManager, MFStartup, MFSTARTUP_FULL, MF_VERSION,
};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

/// Media Foundation must be started once per process before use.
pub fn mf_startup() {
    static START: Once = Once::new();
    START.call_once(|| {
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            log::error!("recorder: MFStartup failed: {e}");
        }
    });
}

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    /// The same device for Windows Graphics Capture.
    pub winrt_device: IDirect3DDevice,
    /// The same device for Media Foundation's hardware encoder.
    pub manager: IMFDXGIDeviceManager,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        mf_startup();
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.expect("D3D11CreateDevice returned no device");
        let context = context.expect("D3D11CreateDevice returned no context");
        // The encoder uses the device from its own threads.
        unsafe {
            let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        }
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?)? }.cast()?;
        let mut token = 0u32;
        let mut manager = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        }
        let manager = manager.expect("MFCreateDXGIDeviceManager returned no manager");
        unsafe { manager.ResetDevice(&device, token)? };
        Ok(Self { device, context, video_device, video_context, winrt_device, manager })
    }

    fn texture(&self, width: u32, height: u32, format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        Ok(texture.expect("CreateTexture2D returned no texture"))
    }
}

/// How many output frames can be in flight in the encoder at once. The
/// sink writer throttles well below this, so a texture is never
/// overwritten while it is still being encoded.
const OUTPUT_RING: usize = 8;

/// The video processor and its views, rebuilt when the cropped input size
/// changes (window resized, F11).
struct Processor {
    in_width: u32,
    in_height: u32,
    input: ID3D11Texture2D,
    input_view: ID3D11VideoProcessorInputView,
    processor: ID3D11VideoProcessor,
    output_views: Vec<ID3D11VideoProcessorOutputView>,
}

pub struct Scaler {
    pub width: u32,
    pub height: u32,
    fps: u32,
    outputs: Vec<ID3D11Texture2D>,
    next: usize,
    processor: Option<Processor>,
}

impl Scaler {
    pub fn new(gpu: &Gpu, width: u32, height: u32, fps: u32) -> Result<Self> {
        let outputs = (0..OUTPUT_RING)
            .map(|_| gpu.texture(width, height, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { width, height, fps, outputs, next: 0, processor: None })
    }

    fn processor(&mut self, gpu: &Gpu, in_width: u32, in_height: u32) -> Result<&Processor> {
        if self.processor.as_ref().is_some_and(|p| p.in_width == in_width && p.in_height == in_height) {
            return Ok(self.processor.as_ref().expect("checked above"));
        }
        self.processor = None;
        let rate = DXGI_RATIONAL { Numerator: self.fps, Denominator: 1 };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_width,
            InputHeight: in_height,
            OutputFrameRate: rate,
            OutputWidth: self.width,
            OutputHeight: self.height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe { gpu.video_device.CreateVideoProcessorEnumerator(&content)? };
        let processor = unsafe { gpu.video_device.CreateVideoProcessor(&enumerator, 0)? };
        let input = gpu.texture(
            in_width,
            in_height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0),
        )?;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut input_view = None;
        unsafe { gpu.video_device.CreateVideoProcessorInputView(&input, &enumerator, &input_desc, Some(&mut input_view))? };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut output_views = Vec::with_capacity(self.outputs.len());
        for output in &self.outputs {
            let mut view = None;
            unsafe { gpu.video_device.CreateVideoProcessorOutputView(output, &enumerator, &output_desc, Some(&mut view))? };
            output_views.push(view.expect("CreateVideoProcessorOutputView returned no view"));
        }

        // Letterbox: keep the game's shape, centered, black around it.
        let scale = (self.width as f64 / in_width as f64).min(self.height as f64 / in_height as f64);
        let dest_w = ((in_width as f64 * scale).round() as i32 & !1).max(2);
        let dest_h = ((in_height as f64 * scale).round() as i32 & !1).max(2);
        let left = ((self.width as i32 - dest_w) / 2) & !1;
        let top = ((self.height as i32 - dest_h) / 2) & !1;
        let dest = RECT { left, top, right: left + dest_w, bottom: top + dest_h };
        let source = RECT { left: 0, top: 0, right: in_width as i32, bottom: in_height as i32 };
        let black = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } },
        };
        // Input: full-range RGB. Output: BT.709 limited-range YCbCr, what
        // H.264 players assume for HD video.
        let rgb_full = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
        let ycbcr_709_studio = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
        unsafe {
            let vc = &gpu.video_context;
            vc.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dest));
            vc.VideoProcessorSetStreamColorSpace(&processor, 0, &rgb_full);
            vc.VideoProcessorSetOutputColorSpace(&processor, &ycbcr_709_studio);
            vc.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
        }
        self.processor = Some(Processor {
            in_width,
            in_height,
            input,
            input_view: input_view.expect("CreateVideoProcessorInputView returned no view"),
            processor,
            output_views,
        });
        Ok(self.processor.as_ref().expect("just set"))
    }

    /// Crops `crop` out of `source` (the captured window) and scales it into
    /// the next output texture, which is returned.
    pub fn process(&mut self, gpu: &Gpu, source: &ID3D11Texture2D, crop: RECT) -> Result<ID3D11Texture2D> {
        let (w, h) = ((crop.right - crop.left) as u32, (crop.bottom - crop.top) as u32);
        let index = self.next;
        self.next = (self.next + 1) % self.outputs.len();
        let output = self.outputs[index].clone();
        let processor = self.processor(gpu, w, h)?;
        let region = D3D11_BOX {
            left: crop.left as u32,
            top: crop.top as u32,
            front: 0,
            right: crop.right as u32,
            bottom: crop.bottom as u32,
            back: 1,
        };
        unsafe {
            gpu.context.CopySubresourceRegion(&processor.input, 0, 0, 0, 0, source, 0, Some(&region));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(processor.input_view.clone())),
                ..Default::default()
            };
            let streams = [stream];
            let result = gpu.video_context.VideoProcessorBlt(&processor.processor, &processor.output_views[index], 0, &streams);
            let [mut stream] = streams;
            std::mem::ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
        }
        Ok(output)
    }
}
