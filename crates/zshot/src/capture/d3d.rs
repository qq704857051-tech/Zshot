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
use crate::hdr::REFERENCE_WHITE_NITS;

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
    return g_tex.Sample(g_samp, i.uv);
}
"#;

static GPU_SHADERS: Mutex<Option<GpuShaders>> = Mutex::new(None);
static FREEZE_TEXTURE: Mutex<Option<ID3D11Texture2D>> = Mutex::new(None);

#[derive(Clone)]
pub struct GpuShaders {
    generation: u64,
    vs: ID3D11VertexShader,
    ps_tonemap: ID3D11PixelShader,
    ps_copy: ID3D11PixelShader,
    #[allow(dead_code)]
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
        let ps_white_lift = ps_copy.clone();
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

pub(super) struct RestoreGpu<'a>(pub(super) &'a ID3D11DeviceContext);

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

pub struct Gpu {
    pub device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps_tonemap: ID3D11PixelShader,
    ps_copy: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl Gpu {
    pub fn bind(device: &ID3D11Device, shaders: &GpuShaders) -> Self {
        Self {
            device: device.clone(),
            vs: shaders.vs.clone(),
            ps_tonemap: shaders.ps_tonemap.clone(),
            ps_copy: shaders.ps_copy.clone(),
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
        self.blit(ctx, src, &dest, desc.Width, desc.Height, hdr, sdr_white_nits)?;
        Ok(dest)
    }

    fn blit(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        dest: &ID3D11Texture2D,
        w: u32,
        h: u32,
        tonemap: bool,
        sdr_white: f32,
    ) -> Result<()> {
        let _restore = RestoreGpu(ctx);
        unsafe {
            let mut srv = None;
            self.device
                .CreateShaderResourceView(src, None, Some(&mut srv))?;
            let srv = srv.ok_or_else(|| Error::new("SRV"))?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(dest, None, Some(&mut rtv))?;
            let rtv: ID3D11RenderTargetView = rtv.ok_or_else(|| Error::new("RTV"))?;

            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.VSSetShader(&self.vs, None);
            if tonemap {
                ctx.PSSetShader(&self.ps_tonemap, None);
                // constant buffer: 16 bytes
                let cb_data = [sdr_white.max(1.0), 0.0, 0.0, 0.0];
                let cb = create_cbuffer(&self.device, &cb_data)?;
                ctx.PSSetConstantBuffers(0, Some(&[Some(cb)]));
            } else {
                ctx.PSSetShader(&self.ps_copy, None);
            }
            ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.IASetPrimitiveTopology(
                windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            );
            ctx.Draw(3, 0);
            let _ = REFERENCE_WHITE_NITS;
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

    pub fn read_bgra(
        &self,
        ctx: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
    ) -> Result<(Vec<u8>, u32, u32, u32)> {
        let desc = texture_desc(src);
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
            let mut pixels = slice.to_vec();
            ctx.Unmap(&staging, 0);
            if desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT {
                pixels = fp16_to_bgra(&pixels, desc.Width, desc.Height, stride);
                return Ok((pixels, desc.Width, desc.Height, desc.Width * 4));
            }
            Ok((pixels, desc.Width, desc.Height, stride))
        }
    }

    pub fn read_bgra_region(
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
        }
        self.read_bgra(ctx, &staging)
    }
}

fn fp16_to_bgra(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let i = y * stride as usize + x * 8;
            if i + 7 >= src.len() {
                continue;
            }
            let r = half_to_f32(u16::from_le_bytes([src[i], src[i + 1]]));
            let g = half_to_f32(u16::from_le_bytes([src[i + 2], src[i + 3]]));
            let b = half_to_f32(u16::from_le_bytes([src[i + 4], src[i + 5]]));
            let a = half_to_f32(u16::from_le_bytes([src[i + 6], src[i + 7]]));
            let o = (y * width as usize + x) * 4;
            // CPU path is export-only (PNG/DIB), not the overlay blit.
            out[o] = crate::hdr::tonemap_srgb8(b, 80.0);
            out[o + 1] = crate::hdr::tonemap_srgb8(g, 80.0);
            out[o + 2] = crate::hdr::tonemap_srgb8(r, 80.0);
            out[o + 3] = (a.clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    out
}

fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let fbits = if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            let mut m = mant;
            let mut e = 127 - 15;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3ff;
            (sign << 31) | ((e + 1) << 23) | (m << 13)
        }
    } else if exp == 31 {
        (sign << 31) | 0x7f800000 | (mant << 13)
    } else {
        (sign << 31) | ((exp + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(fbits)
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
        let slice = std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
        device.CreateVertexShader(slice, None, Some(&mut vs))?;
    }
    let _blob = blob; // drop after Create*Shader
    vs.ok_or_else(|| Error::new("CreateVertexShader"))
}

fn compile_ps(device: &ID3D11Device, entry: &str) -> Result<ID3D11PixelShader> {
    let blob = compile(entry, "ps_5_0")?;
    let mut ps = None;
    unsafe {
        let slice = std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
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

fn create_cbuffer(device: &ID3D11Device, data: &[f32; 4]) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Buffer> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_CONSTANT_BUFFER, D3D11_BUFFER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DYNAMIC,
        D3D11_CPU_ACCESS_WRITE,
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
