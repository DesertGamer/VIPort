//! RGB9E5 textures a host library writes through unordered access: the
//! title's upscaled HDR colour, which Grand Theft Auto VI keeps in RGB9E5
//! (an RT|UAV texture the FidelityFX upscaler writes on the console). The
//! host makes such textures (castable to the 32-bit formats,
//! `resource::castable`) and samples them, but has no typed unordered view
//! of RGB9E5, so the library cannot write one. It writes a stand-in
//! instead, an FP16 texture of the same size, which one compute pass then
//! packs into the title texture through an R32_UINT view of it.
//!
//! shortcut: 2D textures with one mip and layer, castable, only (the
//! upscaler's output). Others get no stand-in and the library writes them
//! as before. Upgrade when a title writes another shape this way.

use std::sync::{Arc, OnceLock};

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use crate::host::{HostList, host, transition};
use crate::resource::HostTexture;

const RGB9E5: u32 = 67;
const R32_UINT: u32 = 42;
const UNORDERED_ACCESS: u32 = 0x8;

/// The FP16 texture a library writes in place of an RGB9E5 one, and the
/// views the pack reads it and writes the title texture through.
pub(crate) struct StandIn {
    /// In the unordered-access state between packs.
    pub(crate) float: ID3D12Resource,
    /// Shader visible: the stand-in's shader view, then the title
    /// texture's R32_UINT unordered view.
    views: ID3D12DescriptorHeap,
    size: [u32; 2],
}

// SAFETY: D3D12 objects are free-threaded; the rest is data.
unsafe impl Send for StandIn {}
// SAFETY: as above.
unsafe impl Sync for StandIn {}

/// `target`'s stand-in, made on first use; `None` if it needs none (not
/// RGB9E5, or the host writes RGB9E5 itself) or cannot have one.
pub(crate) fn stand_in(target: &Arc<HostTexture>) -> Option<&StandIn> {
    // SAFETY: a live resource.
    let format = unsafe { target.texture.GetDesc() }.Format.0 as u32;
    if format != RGB9E5 || host_writes_rgb9e5() {
        return None;
    }
    target.rgb9e5.get_or_init(|| make(target)).as_ref()
}

/// Whether the host writes RGB9E5 through typed unordered views (no PC
/// GPU did when this was written; asked once).
fn host_writes_rgb9e5() -> bool {
    static WRITES: OnceLock<bool> = OnceLock::new();
    *WRITES.get_or_init(|| {
        let Some(host) = host() else { return false };
        let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT { Format: DXGI_FORMAT_R9G9B9E5_SHAREDEXP, ..Default::default() };
        // SAFETY: the feature's own structure and size.
        let asked = unsafe {
            host.device.CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, (&raw mut support).cast(), size_of_val(&support) as u32)
        };
        asked.is_ok()
            && support.Support1.contains(D3D12_FORMAT_SUPPORT1_TYPED_UNORDERED_ACCESS_VIEW)
            && support.Support2.contains(D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE)
    })
}

fn make(target: &HostTexture) -> Option<StandIn> {
    let host = host()?;
    // SAFETY: a live resource.
    let td = unsafe { target.texture.GetDesc() };
    if td.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D || td.MipLevels != 1 || td.DepthOrArraySize != 1 {
        once("an RGB9E5 texture written through unordered access is not one 2D level: no stand-in");
        return None;
    }
    if !target.castable.contains(&R32_UINT) {
        once("an RGB9E5 texture written through unordered access is not castable to R32_UINT: no stand-in");
        return None;
    }
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC {
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Alignment: 0,
        ..td
    };
    let mut float: Option<ID3D12Resource> = None;
    // SAFETY: plain creation; our out pointer.
    let made = unsafe {
        host.device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, None, &mut float)
    };
    if let Err(e) = made {
        tracing::warn!(error = %e, ?desc, "no FP16 stand-in for an RGB9E5 texture");
        return None;
    }
    let float = float?;
    crate::host::name(&float, || format!("rgb9e5 stand-in {}x{}", td.Width, td.Height));
    let heap_desc = D3D12_DESCRIPTOR_HEAP_DESC {
        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
        NumDescriptors: 2,
        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
        NodeMask: 0,
    };
    // SAFETY: plain creation.
    let views: ID3D12DescriptorHeap = unsafe { host.device.CreateDescriptorHeap(&heap_desc) }.ok()?;
    let uav = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT(R32_UINT as i32),
        ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 { Texture2D: D3D12_TEX2D_UAV { MipSlice: 0, PlaneSlice: 0 } },
    };
    // SAFETY: live textures; both slots are in our heap.
    unsafe {
        let at = views.GetCPUDescriptorHandleForHeapStart();
        let step = host.device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
        host.device.CreateShaderResourceView(&float, None, at);
        host.device.CreateUnorderedAccessView(&target.texture, None, Some(&uav), D3D12_CPU_DESCRIPTOR_HANDLE { ptr: at.ptr + step });
    }
    once("an RGB9E5 texture written through unordered access: an FP16 stand-in, packed into it after each write");
    Some(StandIn { float, views, size: [td.Width as u32, td.Height] })
}

impl StandIn {
    /// Records into `list` the pack of the stand-in into `target`, which it
    /// leaves in the unordered-access state (as the library would have).
    /// The list's compute root signature, pipeline and heaps are ours after
    /// it.
    pub(crate) fn pack(&self, list: &HostList, target: &Arc<HostTexture>) -> bool {
        let Some(p) = packer() else { return false };
        list.keep(&self.float);
        list.keep(&self.views);
        let mut before = vec![transition(&self.float, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE)];
        before.extend(list.move_texture(target, UNORDERED_ACCESS));
        let l = &list.list;
        // SAFETY: an open list; live resources in the states named; the
        // heap holds both views the root signature's table names.
        unsafe {
            l.ResourceBarrier(&before);
            l.SetDescriptorHeaps(&[Some(self.views.clone())]);
            l.SetComputeRootSignature(&p.root);
            l.SetPipelineState(&p.pso);
            l.SetComputeRootDescriptorTable(0, self.views.GetGPUDescriptorHandleForHeapStart());
            l.SetComputeRoot32BitConstants(1, 2, self.size.as_ptr().cast(), 0);
            l.Dispatch(self.size[0].div_ceil(8), self.size[1].div_ceil(8), 1);
            l.ResourceBarrier(&[
                transition(&self.float, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
                uav_barrier(&target.texture),
            ]);
        }
        true
    }
}

fn uav_barrier(resource: &ID3D12Resource) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                // SAFETY: a borrowed pointer the barrier does not release.
                pResource: unsafe { std::mem::transmute_copy(resource) },
            }),
        },
    }
}

fn once(what: &'static str) {
    static SEEN: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&what) {
        tracing::info!("{what}");
        seen.push(what);
    }
}

/// FP16 colour to RGB9E5 (DXGI's rules: 9-bit mantissas, a shared 5-bit
/// exponent biased by 15, negatives and NaN as 0, up to 65408), rounded to
/// nearest.
const SOURCE: &str = r#"
#define RS "DescriptorTable(SRV(t0), UAV(u0)), RootConstants(num32BitConstants=2, b0)"
Texture2D<float4> color : register(t0);
RWTexture2D<uint> packed : register(u0);
cbuffer Size : register(b0) { uint width; uint height; };
uint pack(float3 rgb) {
    rgb = clamp(rgb, 0.0, 65408.0);
    float largest = max(rgb.r, max(rgb.g, rgb.b));
    // floor(log2(largest)) + 16, from the float's exponent, at least 0.
    uint exponent = ((asuint(max(largest, exp2(-16.0))) >> 23) & 255u) - 111u;
    float scale = exp2(24.0 - float(exponent));
    if (floor(largest * scale + 0.5) >= 512.0) {
        exponent++;
        scale *= 0.5;
    }
    uint3 m = uint3(floor(rgb * scale + 0.5));
    return m.r | m.g << 9 | m.b << 18 | exponent << 27;
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= width || id.y >= height) return;
    packed[id.xy] = pack(color.Load(int3(id.xy, 0)).rgb);
}
"#;

struct Packer {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Packer {}
// SAFETY: as above.
unsafe impl Sync for Packer {}

fn packer() -> Option<&'static Packer> {
    static MADE: OnceLock<Option<Packer>> = OnceLock::new();
    MADE.get_or_init(|| match compile() {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, "no RGB9E5 pack: what a library writes through a stand-in does not reach the title texture");
            None
        }
    })
    .as_ref()
}

fn compile() -> windows::core::Result<Packer> {
    let device = &host().ok_or_else(windows::core::Error::empty)?.device;
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(SOURCE.as_ptr().cast(), SOURCE.len(), s!("rgb9e5"), None, None, s!("main"), s!("cs_5_1"), 0, 0, &mut code, Some(&mut errors))
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe { std::slice::from_raw_parts(errors.GetBufferPointer().cast::<u8>(), errors.GetBufferSize()) };
            tracing::warn!("RGB9E5 pack: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    let code = code.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: the blob's own bytes; the root signature is inside the shader.
    unsafe {
        let bytes = std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
            ..Default::default()
        })?;
        Ok(Packer { root, pso })
    }
}
