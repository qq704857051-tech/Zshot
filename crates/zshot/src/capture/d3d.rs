//! Shared D3D11 helpers, GPU copies, HDR tonemap pass, staging readback.

use std::sync::Mutex;

use windows::core::{Interface, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SAMPLER_DESC,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIFactory1};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

use crate::error::{Error, Result};

const TONEMAP_HLSL: &str = r#"
Texture2D<float4> g_tex : register(t0);
SamplerState g_samp : register(s0);
cbuffer CB : register(b0) { float sdrWhite; float3 pad; }

float oetf(float c) {
    float x = abs(c);
    if (x <= 0.0031308) return c * 12.92;
    return sign(c) * (1.055 * pow(x, 1.0/2.4) - 0.055);
}

struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

VSOut vs_main(uint id : SV_VertexID) {
    VSOut o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

float4 ps_main(VSOut i) : SV_Target {
    float4 c = g_tex.Sample(g_samp, i.uv);
    float scale = 80.0 / max(sdrWhite, 1.0);
    float3 l = c.rgb * scale;
    return float4(saturate(oetf(l.r)), saturate(oetf(l.g)), saturate(oetf(l.b)), 1);
}

float4 ps_copy(VSOut i) : SV_Target {
    // Alpha is forced opaque: desktop sources can hand back 0 and the PNG encoder
    // writes straight-alpha BGRA, which would save a fully transparent image.
    return float4(g_tex.Sample(g_samp, i.uv).rgb, 1);
}

float4 ps_white_lift(VSOut i) : SV_Target {
    float4 c = g_tex.Sample(g_samp, i.uv);
    float scale = sdrWhite / 80.0;
    return float4(c.rgb * scale, 1);
}
"#;

enum BlitMode {
    Copy,
    Tonemap,
    WhiteLift,
}

static GPU_SHADERS: Mutex<Option<GpuShaders>> = Mutex::new(None);
static FREEZE_TEXTURE: Mutex<Option<ID3D11Texture2D>> = Mutex::new(None);

#[derive(Clone)]
pub struct GpuShaders {
    generation: u64,
    vs: ID3D11VertexShader,
    ps_tonemap: ID3D11PixelShader,
    ps_copy: ID3D11PixelShader,
    ps_white_lift: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl GpuShaders {
    pub fn get_or_compile(generation: u64, device: &ID3D11Device) -> Result<Self> {
        let mut slot = GPU_SHADERS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = slot.as_ref() {
            if existing.generation == generation {
                return Ok(existing.clone());
            }
        }
        let compiled = Self::compile(generation, device)?;
        *slot = Some(compiled.clone());
        Ok(compiled)
    }

    fn compile(generation: u64, device: &ID3D11Device) -> Result<Self> {
        let vs = compile_vs(device)?;
        let ps_tonemap = compile_ps(device, "ps_main")?;
        let ps_copy = compile_ps(device, "ps_copy")?;
        let ps_white_lift = compile_ps(device, "ps_white_lift")?;
        let sampler = unsafe {
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut s = None;
            device.CreateSamplerState(&desc, Some(&mut s))?;
            s.ok_or_else(|| Error::new("CreateSamplerState"))?
        };
        Ok(Self {
            generation,
            vs,
            ps_tonemap,
            ps_copy,
            ps_white_lift,
            sampler,
        })
    }
}

pub fn drop_gpu_resources() {
    *GPU_SHADERS.lock().unwrap_or_else(|p| p.into_inner()) = None;
    *FREEZE_TEXTURE.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

pub struct RestoreGpu<'a>(pub &'a ID3D11DeviceContext);

impl Drop for RestoreGpu<'_> {
    fn drop(&mut self) {
        unsafe {
            self.0
                .PSSetShaderResources(0, Some(&[None::<ID3D11ShaderResourceView>]));
            self.0.OMSetRenderTargets(None, None);
            self.0.ClearState();
        }
    }
}

#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps_tonemap: ID3D11PixelShader,
    ps_copy: ID3D11PixelShader,
    ps_white_lift: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl Gpu {
    pub fn bind(device: &ID3D11Device, shaders: &GpuShaders) -> Self {
        Self {
            device: device.clone(),
            vs: shaders.vs.clone(),
            ps_tonemap: shaders.ps_tonemap.clone(),
            ps_copy: shaders.ps_copy.clone(),
            ps_white_lift: shaders.ps_white_lift.clone(),
            sampler: shaders.sampler.clone(),
        }
    }

    pub fn winrt_device(&self) -> Result<windows::Graphics::DirectX::Direct3D11::IDirect3DDevice> {
        let dxgi: IDXGIDevice = self.device.cast()?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
        inspectable.cast().map_err(Into::into)
    }

    pub fn copy_to_owned(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
    ) -> Result<ID3D11Texture2D> {
        let desc = texture_desc(src);
        let owned = create_texture(
            &self.device,
            desc.Width,
            desc.Height,
            desc.Format,
            D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
            D3D11_USAGE_DEFAULT,
            0,
        )?;
        unsafe {
            ctx.CopyResource(&owned, src);
        }
        Ok(owned)
    }

    /// GPU tonemap FP16 scRGB → BGRA8. SDR BGRA sources are GPU-copied, never a CPU round-trip.
    pub fn to_sdr_bgra(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        sdr_white_nits: f32,
    ) -> Result<ID3D11Texture2D> {
        let desc = texture_desc(src);
        let dest = create_texture(
            &self.device,
            desc.Width,
            desc.Height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
            D3D11_USAGE_DEFAULT,
            0,
        )?;
        let hdr = desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT;
        let mode = if hdr {
            BlitMode::Tonemap
        } else {
            BlitMode::Copy
        };
        self.blit(
            ctx,
            src,
            &dest,
            (0, 0, desc.Width, desc.Height),
            mode,
            sdr_white_nits,
        )?;
        Ok(dest)
    }

    /// Draw SDR BGRA8 (or FP16 SDR) into an FP16 composite with `rgb * (sdrWhite/80)`.
    /// Must not CopySubresourceRegion across formats.
    pub(super) fn white_lift_into(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        dest: &ID3D11Texture2D,
        dst_x: u32,
        dst_y: u32,
        sdr_white: f32,
    ) -> Result<()> {
        let desc = texture_desc(src);
        self.blit(
            ctx,
            src,
            dest,
            (dst_x, dst_y, desc.Width, desc.Height),
            BlitMode::WhiteLift,
            sdr_white,
        )
    }

    fn blit(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        dest: &ID3D11Texture2D,
        dest_rect: (u32, u32, u32, u32),
        mode: BlitMode,
        sdr_white: f32,
    ) -> Result<()> {
        let _restore = RestoreGpu(ctx);
        let (dst_x, dst_y, w, h) = dest_rect;
        unsafe {
            let mut srv = None;
            self.device
                .CreateShaderResourceView(src, None, Some(&mut srv))?;
            let srv = srv.ok_or_else(|| Error::new("SRV"))?;
            let mut rtv = None;
            self.device
                .CreateRenderTargetView(dest, None, Some(&mut rtv))?;
            let rtv: ID3D11RenderTargetView = rtv.ok_or_else(|| Error::new("RTV"))?;

            let vp = D3D11_VIEWPORT {
                TopLeftX: dst_x as f32,
                TopLeftY: dst_y as f32,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.VSSetShader(&self.vs, None);
            let ps = match mode {
                BlitMode::Tonemap => &self.ps_tonemap,
                BlitMode::WhiteLift => &self.ps_white_lift,
                BlitMode::Copy => &self.ps_copy,
            };
            ctx.PSSetShader(ps, None);
            if !matches!(mode, BlitMode::Copy) {
                let cb_data = [sdr_white.max(1.0), 0.0, 0.0, 0.0];
                let cb = create_cbuffer(&self.device, &cb_data)?;
                ctx.PSSetConstantBuffers(0, Some(&[Some(cb)]));
            }
            ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.IASetPrimitiveTopology(
                windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            );
            ctx.Draw(3, 0);
        }
        Ok(())
    }

    /// Small GPU copy + sample. HDR FP16 uses the FP16 detector so exclusive
    /// black frames are not skipped.
    pub fn texture_is_near_black(&self, ctx: &ID3D11DeviceContext, src: &ID3D11Texture2D) -> bool {
        let desc = texture_desc(src);
        let w = desc.Width.clamp(1, 64);
        let h = desc.Height.clamp(1, 64);
        match self.read_raw_region(ctx, src, 0, 0, w, h) {
            Ok((px, ww, hh, stride)) => {
                if desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT {
                    crate::black::is_near_black_fp16(&px, ww, hh, stride as usize)
                } else {
                    crate::black::is_near_black_bgra(&px, ww, hh, stride as usize)
                }
            }
            Err(_) => false,
        }
    }

    fn read_raw_region(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(Vec<u8>, u32, u32, u32)> {
        let desc = texture_desc(src);
        let w = w.min(desc.Width.saturating_sub(x)).max(1);
        let h = h.min(desc.Height.saturating_sub(y)).max(1);
        let staging = create_texture(
            &self.device,
            w,
            h,
            desc.Format,
            Default::default(),
            D3D11_USAGE_STAGING,
            D3D11_CPU_ACCESS_READ.0 as u32,
        )?;
        unsafe {
            let box_ = D3D11_BOX {
                left: x,
                top: y,
                front: 0,
                right: x + w,
                bottom: y + h,
                back: 1,
            };
            ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, src, 0, Some(&box_));
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let stride = mapped.RowPitch;
            let size = stride * h;
            let slice = std::slice::from_raw_parts(mapped.pData as *const u8, size as usize);
            let pixels = slice.to_vec();
            ctx.Unmap(&staging, 0);
            Ok((pixels, w, h, stride))
        }
    }

    /// BGRA8 only — every caller reads back `CapturedDesktop::sdr_display`, which
    /// `to_sdr_bgra` has already tonemapped.
    pub fn read_bgra(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
    ) -> Result<(Vec<u8>, u32, u32, u32)> {
        let desc = texture_desc(src);
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(Error::new("read_bgra expects a BGRA8 texture"));
        }
        let staging = create_texture(
            &self.device,
            desc.Width,
            desc.Height,
            desc.Format,
            Default::default(),
            D3D11_USAGE_STAGING,
            D3D11_CPU_ACCESS_READ.0 as u32,
        )?;
        unsafe {
            ctx.CopyResource(&staging, src);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let stride = mapped.RowPitch;
            let size = stride * desc.Height;
            let slice = std::slice::from_raw_parts(mapped.pData as *const u8, size as usize);
            let pixels = slice.to_vec();
            ctx.Unmap(&staging, 0);
            Ok((pixels, desc.Width, desc.Height, stride))
        }
    }

    /// One staging texture, one copy. Called per mouse-move for the magnifier and
    /// once per export for the full crop, so a second round-trip is not free.
    pub fn read_bgra_region(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<(Vec<u8>, u32, u32, u32)> {
        if texture_desc(src).Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(Error::new("read_bgra_region expects a BGRA8 texture"));
        }
        self.read_raw_region(ctx, src, x, y, w, h)
    }
}

pub fn texture_desc(tex: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    desc
}

pub fn create_texture(
    device: &ID3D11Device,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    bind: windows::Win32::Graphics::Direct3D11::D3D11_BIND_FLAG,
    usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE,
    cpu: u32,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: usage,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.ok_or_else(|| Error::new("CreateTexture2D"))
}

fn compile_vs(device: &ID3D11Device) -> Result<ID3D11VertexShader> {
    let blob = compile("vs_main", "vs_5_0")?;
    let mut vs = None;
    unsafe {
        let slice =
            std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
        device.CreateVertexShader(slice, None, Some(&mut vs))?;
    }
    let _blob = blob; // drop after Create*Shader
    vs.ok_or_else(|| Error::new("CreateVertexShader"))
}

fn compile_ps(device: &ID3D11Device, entry: &str) -> Result<ID3D11PixelShader> {
    let blob = compile(entry, "ps_5_0")?;
    let mut ps = None;
    unsafe {
        let slice =
            std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
        device.CreatePixelShader(slice, None, Some(&mut ps))?;
    }
    let _blob = blob; // drop after Create*Shader
    ps.ok_or_else(|| Error::new("CreatePixelShader"))
}

fn compile(entry: &str, target: &str) -> Result<windows::Win32::Graphics::Direct3D::ID3DBlob> {
    let mut blob = None;
    let mut err = None;
    let entry_z = std::ffi::CString::new(entry).unwrap();
    let target_z = std::ffi::CString::new(target).unwrap();
    unsafe {
        D3DCompile(
            TONEMAP_HLSL.as_ptr() as *const _,
            TONEMAP_HLSL.len(),
            None,
            None,
            None,
            PCSTR(entry_z.as_ptr() as *const u8),
            PCSTR(target_z.as_ptr() as *const u8),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut err),
        )
        .map_err(|e| {
            let msg = if let Some(err) = err {
                let s = std::slice::from_raw_parts(
                    err.GetBufferPointer() as *const u8,
                    err.GetBufferSize(),
                );
                String::from_utf8_lossy(s).into_owned()
            } else {
                e.to_string()
            };
            Error::new(format!("D3DCompile {entry}: {msg}"))
        })?;
    }
    blob.ok_or_else(|| Error::new("D3DCompile blob"))
}

fn create_cbuffer(
    device: &ID3D11Device,
    data: &[f32; 4],
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Buffer> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_CONSTANT_BUFFER, D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_WRITE,
        D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DYNAMIC,
    };
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: 16,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        ..Default::default()
    };
    let srd = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr() as *const _,
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    };
    let mut buf = None;
    unsafe { device.CreateBuffer(&desc, Some(&srd), Some(&mut buf))? };
    buf.ok_or_else(|| Error::new("cbuffer"))
}

pub fn dxgi_factory() -> Result<IDXGIFactory1> {
    use windows::Win32::Graphics::Dxgi::CreateDXGIFactory1;
    unsafe { CreateDXGIFactory1().map_err(Into::into) }
}

#[cfg(test)]
mod golden {
    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        D3D11_SUBRESOURCE_DATA,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

    fn try_warp_device() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        let mut device = None;
        let mut ctx = None;
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .ok()?;
        }
        Some((device?, ctx?))
    }

    fn solid_bgra(device: &ID3D11Device, b: u8, g: u8, r: u8, a: u8) -> Result<ID3D11Texture2D> {
        let pixels = [b, g, r, a];
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 1,
            Height: 1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let srd = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr() as *const _,
            SysMemPitch: 4,
            SysMemSlicePitch: 0,
        };
        let mut tex = None;
        unsafe { device.CreateTexture2D(&desc, Some(&srd), Some(&mut tex))? };
        tex.ok_or_else(|| Error::new("CreateTexture2D"))
    }

    #[test]
    fn gpu_golden_sdr_lift_channel_error_le_1() {
        // WARP ships with Windows; a silent `return` here would let this pass
        // without ever running the shader.
        let (device, ctx) = try_warp_device().expect("WARP device unavailable");
        let shaders = GpuShaders::compile(0, &device).expect("compile shaders");
        let gpu = Gpu::bind(&device, &shaders);
        let src = solid_bgra(&device, 40, 80, 200, 255).expect("src");
        let composite = create_texture(
            &device,
            1,
            1,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
            D3D11_USAGE_DEFAULT,
            0,
        )
        .expect("fp16 composite");
        let sdr_white = 203.0;
        gpu.white_lift_into(&ctx, &src, &composite, 0, 0, sdr_white)
            .expect("white lift");
        let display = gpu
            .to_sdr_bgra(&ctx, &composite, sdr_white)
            .expect("tonemap");
        let (px, _, _, _) = gpu.read_bgra(&ctx, &display).expect("readback");
        let lift = |c: u8| {
            crate::hdr::white_level_adjust(
                c as f32 / 255.0,
                sdr_white,
                crate::hdr::REFERENCE_WHITE_NITS,
            )
        };
        let expected = [
            crate::hdr::tonemap_srgb8(lift(40), sdr_white),
            crate::hdr::tonemap_srgb8(lift(80), sdr_white),
            crate::hdr::tonemap_srgb8(lift(200), sdr_white),
        ];
        for i in 0..3 {
            let err = (px[i] as i16 - expected[i] as i16).abs();
            assert!(
                err <= 1,
                "channel {i}: got={} expected={} err={err}",
                px[i],
                expected[i]
            );
        }
    }
}
