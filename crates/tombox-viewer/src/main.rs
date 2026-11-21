//! tombox-viewer: run translated console shaders on the host GPU and look at what they draw.
//!
//! ```text
//! tombox-viewer --vs <vs.dxbc> --ps <ps.dxbc> [--model sphere|cube|torus|quad] [--size N] [--out DIR] [--fill identity|ones]
//! tombox-viewer --cs <cs.dxbc> [--groups X,Y,Z] [--out DIR]
//! ```
//!
//! Each container is extracted and translated exactly as the runtime does it, compiled with
//! DXC, and run on a procedural model. Every render target (or written UAV image) is read
//! back and saved as a PNG, with statistics (coverage, NaN/Inf counts) printed.
//!
//! What this proves and what it does not: the shaders are real game code running on the PC
//! GPU through the translator. Their inputs are made up — the harness does not know the
//! game's constant-buffer layouts or descriptor contents. Constant and structured buffers are
//! filled with repeated 4x4 identity matrices (so a matrix loaded at a 64-byte boundary
//! passes positions through), textures are checkerboards, samplers are linear. A picture
//! shows that the shader executes and produces finite, structured output; it is not the image
//! the game would draw.
//!
//! Shaders without a root signature (descriptor-heap mode, see tombox-xshader `heap_mode`)
//! get a heap built from the translator's binding report: every entry SGPR the code uses as a
//! table base is given its own heap region, and each reported (offset, type) gets a
//! descriptor of that type. Heap indices are masked to the heap size (a test-only translation
//! option) so a made-up index cannot leave the heap.

use std::path::{Path, PathBuf};
use tombox_xshader::hlsl::{translate_with, Options, Translated};
use tombox_xshader::meta::SigElement;
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::System::Threading::*;

const HEAP: u32 = 2048; // descriptors per heap (power of two: indices are masked)
const REGION: u32 = 256; // heap entries given to each table-base SGPR

type R<T> = Result<T, String>;

fn hr<T>(r: windows::core::Result<T>, what: &str) -> R<T> {
    r.map_err(|e| format!("{what}: {e}"))
}

// ---------------------------------------------------------------------------------------
// Models

struct Model {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    idx: Vec<u32>,
}

fn model(kind: &str) -> R<Model> {
    let mut m = Model { pos: vec![], nrm: vec![], uv: vec![], idx: vec![] };
    match kind {
        "sphere" | "torus" => {
            let (rings, segs) = (32u32, 48u32);
            for i in 0..=rings {
                for j in 0..=segs {
                    let (u, v) = (j as f32 / segs as f32, i as f32 / rings as f32);
                    let (th, ph) = (u * std::f32::consts::TAU, v * std::f32::consts::PI);
                    let (p, n) = if kind == "sphere" {
                        let n = [ph.sin() * th.cos(), ph.cos(), ph.sin() * th.sin()];
                        ([n[0] * 0.7, n[1] * 0.7, n[2] * 0.7], n)
                    } else {
                        let ph2 = v * std::f32::consts::TAU;
                        let (r0, r1) = (0.5f32, 0.2f32);
                        let c = [th.cos(), 0.0, th.sin()];
                        let n = [ph2.cos() * c[0], ph2.sin(), ph2.cos() * c[2]];
                        ([c[0] * r0 + n[0] * r1, n[1] * r1, c[2] * r0 + n[2] * r1], n)
                    };
                    m.pos.push(p);
                    m.nrm.push(n);
                    m.uv.push([u, v]);
                }
            }
            for i in 0..rings {
                for j in 0..segs {
                    let a = i * (segs + 1) + j;
                    let b = a + segs + 1;
                    m.idx.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
                }
            }
        }
        "cube" => {
            let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
                ([1., 0., 0.], [0., 1., 0.], [0., 0., 1.]),
                ([-1., 0., 0.], [0., 1., 0.], [0., 0., -1.]),
                ([0., 1., 0.], [0., 0., 1.], [1., 0., 0.]),
                ([0., -1., 0.], [0., 0., -1.], [1., 0., 0.]),
                ([0., 0., 1.], [0., 1., 0.], [-1., 0., 0.]),
                ([0., 0., -1.], [0., 1., 0.], [1., 0., 0.]),
            ];
            for (n, up, rt) in faces {
                let base = m.pos.len() as u32;
                for (s, t) in [(-1f32, -1f32), (1., -1.), (1., 1.), (-1., 1.)] {
                    let p: Vec<f32> = (0..3).map(|k| (n[k] + rt[k] * s + up[k] * t) * 0.45).collect();
                    m.pos.push([p[0], p[1], p[2]]);
                    m.nrm.push(n);
                    m.uv.push([(s + 1.) / 2., (t + 1.) / 2.]);
                }
                m.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        "quad" => {
            for (x, y) in [(-1f32, -1f32), (1., -1.), (1., 1.), (-1., 1.)] {
                m.pos.push([x, y, 0.0]);
                m.nrm.push([0., 0., -1.]);
                m.uv.push([(x + 1.) / 2., (1. - y) / 2.]);
            }
            m.idx.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        }
        other => return Err(format!("unknown model {other} (sphere, cube, torus, quad)")),
    }
    // Tilt for a three-quarter view and push into clip space (z in [0.2, 0.8], w = 1).
    let (a, b) = (0.5f32, 0.6f32);
    for p in m.pos.iter_mut().chain(m.nrm.iter_mut()) {
        let (x, y, z) = (p[0], p[1], p[2]);
        let (x, z) = (x * a.cos() + z * a.sin(), -x * a.sin() + z * a.cos());
        let (y, z) = (y * b.cos() - z * b.sin(), y * b.sin() + z * b.cos());
        *p = [x, y, z];
    }
    if kind != "quad" {
        for p in m.pos.iter_mut() {
            p[2] = 0.5 + p[2] * 0.4;
        }
    } else {
        for p in m.pos.iter_mut() {
            p[2] = 0.5;
        }
    }
    Ok(m)
}

/// Vertex data for one input-signature element, by semantic.
fn attribute(e: &SigElement, m: &Model, v: usize) -> [u32; 4] {
    let f = |a: [f32; 4]| a.map(f32::to_bits);
    let name = e.name.to_ascii_uppercase();
    let (p, n, t) = (m.pos[v], m.nrm[v], m.uv[v]);
    if e.comp_type == 1 || e.comp_type == 2 {
        // Integer attributes (indices, packed data): small, well-defined values.
        return [v as u32 % 4, 0, 0, 0];
    }
    match (name.as_str(), e.index) {
        ("POSITION", _) | ("ATTRIBUTE", 0) => f([p[0], p[1], p[2], 1.0]),
        ("NORMAL", _) | ("ATTRIBUTE", 2) => f([n[0], n[1], n[2], 1.0]),
        ("TANGENT", _) | ("ATTRIBUTE", 1) => f([n[1], -n[0], 0.0, 1.0]),
        ("COLOR", _) => f([t[0], t[1], 1.0 - t[0], 1.0]),
        ("TEXCOORD", _) | ("ATTRIBUTE", _) => f([t[0], t[1], t[0], t[1]]),
        _ => f([0.0, 0.0, 0.0, 1.0]),
    }
}

// ---------------------------------------------------------------------------------------
// Shader loading

struct Stage {
    name: String,
    t: Translated,
    dxil: Vec<u8>,
    input: Vec<SigElement>,
    output: Vec<SigElement>,
}

fn load(path: &Path) -> R<Stage> {
    let blob = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ex = tombox_xshader::extract(&blob)?;
    let t = translate_with(&ex, &Options { heap_index_mask: Some(HEAP - 1) })?;
    let profile = tombox_xshader::dxc::profile_for(t.profile);
    let dxil = tombox_xshader::dxc::compile(&t.hlsl, &profile).map_err(|e| {
        let _ = std::fs::write(path.with_extension("failed.hlsl"), &t.hlsl);
        format!("{}: DXC: {e}", path.display())
    })?;
    println!("{}: {:?} {} -> {profile}, {} HLSL bytes, heap={} ({} bindings, {} static samplers)",
        path.display(), ex.stage, ex.entry.clone().unwrap_or_default(), t.hlsl.len(), t.heap, t.heap_bindings.len(), t.static_samplers.len());
    Ok(Stage { name: ex.entry.clone().unwrap_or_default(), t, dxil, input: ex.input_sig.clone(), output: ex.output_sig.clone() })
}

// ---------------------------------------------------------------------------------------
// D3D12 helpers

struct Gpu {
    dev: ID3D12Device,
    queue: ID3D12CommandQueue,
    alloc: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    event: HANDLE,
    keep: Vec<ID3D12Resource>,
}

fn heap_props(t: D3D12_HEAP_TYPE) -> D3D12_HEAP_PROPERTIES {
    D3D12_HEAP_PROPERTIES { Type: t, CreationNodeMask: 1, VisibleNodeMask: 1, ..Default::default() }
}

fn buffer_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    }
}

fn transition(res: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: unsafe { std::mem::transmute_copy(res) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

impl Gpu {
    fn new() -> R<Gpu> {
        unsafe {
            let mut dev: Option<ID3D12Device> = None;
            hr(D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut dev), "D3D12CreateDevice")?;
            let dev = dev.ok_or("no device")?;
            let mut sm = D3D12_FEATURE_DATA_SHADER_MODEL { HighestShaderModel: D3D_SHADER_MODEL_6_6 };
            let ok = dev.CheckFeatureSupport(D3D12_FEATURE_SHADER_MODEL, &mut sm as *mut _ as *mut _, std::mem::size_of_val(&sm) as u32);
            if ok.is_err() || sm.HighestShaderModel.0 < D3D_SHADER_MODEL_6_6.0 {
                println!("warning: device reports shader model {:#x}; 6.6 is needed for heap-mode shaders", sm.HighestShaderModel.0);
            }
            let queue: ID3D12CommandQueue = hr(dev.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() }), "queue")?;
            let alloc: ID3D12CommandAllocator = hr(dev.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT), "allocator")?;
            let list: ID3D12GraphicsCommandList = hr(dev.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc, None), "list")?;
            let fence: ID3D12Fence = hr(dev.CreateFence(0, D3D12_FENCE_FLAG_NONE), "fence")?;
            let event = hr(CreateEventA(None, false, false, PCSTR::null()), "event")?;
            Ok(Gpu { dev, queue, alloc, list, fence, fence_value: 0, event, keep: vec![] })
        }
    }

    fn upload(&mut self, data: &[u8]) -> R<ID3D12Resource> {
        unsafe {
            let mut res: Option<ID3D12Resource> = None;
            hr(self.dev.CreateCommittedResource(&heap_props(D3D12_HEAP_TYPE_UPLOAD), D3D12_HEAP_FLAG_NONE,
                &buffer_desc(data.len().max(256) as u64, D3D12_RESOURCE_FLAG_NONE), D3D12_RESOURCE_STATE_GENERIC_READ, None, &mut res), "upload buffer")?;
            let res = res.unwrap();
            let mut p = std::ptr::null_mut();
            hr(res.Map(0, None, Some(&mut p)), "map")?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
            res.Unmap(0, None);
            self.keep.push(res.clone());
            Ok(res)
        }
    }

    fn default_buffer(&mut self, size: u64, uav: bool) -> R<ID3D12Resource> {
        unsafe {
            let mut res: Option<ID3D12Resource> = None;
            let flags = if uav { D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS } else { D3D12_RESOURCE_FLAG_NONE };
            hr(self.dev.CreateCommittedResource(&heap_props(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE,
                &buffer_desc(size, flags), D3D12_RESOURCE_STATE_COMMON, None, &mut res), "buffer")?;
            let res = res.unwrap();
            self.keep.push(res.clone());
            Ok(res)
        }
    }

    fn texture(&mut self, dim: D3D12_RESOURCE_DIMENSION, w: u64, h: u32, depth: u16, fmt: DXGI_FORMAT, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES, clear: Option<&D3D12_CLEAR_VALUE>) -> R<ID3D12Resource> {
        unsafe {
            let desc = D3D12_RESOURCE_DESC {
                Dimension: dim,
                Width: w,
                Height: h,
                DepthOrArraySize: depth,
                MipLevels: 1,
                Format: fmt,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Flags: flags,
                ..Default::default()
            };
            let mut res: Option<ID3D12Resource> = None;
            hr(self.dev.CreateCommittedResource(&heap_props(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE, &desc, state, clear.map(|c| c as *const _), &mut res), "texture")?;
            let res = res.unwrap();
            self.keep.push(res.clone());
            Ok(res)
        }
    }

    /// Execute the recorded list and wait; reports device removal.
    fn run(&mut self) -> R<()> {
        unsafe {
            hr(self.list.Close(), "close")?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast().unwrap())]);
            self.fence_value += 1;
            hr(self.queue.Signal(&self.fence, self.fence_value), "signal")?;
            if self.fence.GetCompletedValue() < self.fence_value {
                hr(self.fence.SetEventOnCompletion(self.fence_value, self.event), "event")?;
                WaitForSingleObject(self.event, 20_000);
            }
            if let Err(e) = self.dev.GetDeviceRemovedReason() {
                return Err(format!("GPU device removed while running the shaders: {e}"));
            }
            hr(self.alloc.Reset(), "alloc reset")?;
            hr(self.list.Reset(&self.alloc, None), "list reset")?;
            Ok(())
        }
    }

    /// Copy a 2D texture (subresource 0) to the CPU: (row pitch, bytes).
    fn readback(&mut self, tex: &ID3D12Resource, state: D3D12_RESOURCE_STATES) -> R<(usize, Vec<u8>)> {
        unsafe {
            let desc = tex.GetDesc();
            let mut fp = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let mut total = 0u64;
            self.dev.GetCopyableFootprints(&desc, 0, 1, 0, Some(&mut fp), None, None, Some(&mut total));
            let mut buf: Option<ID3D12Resource> = None;
            hr(self.dev.CreateCommittedResource(&heap_props(D3D12_HEAP_TYPE_READBACK), D3D12_HEAP_FLAG_NONE,
                &buffer_desc(total, D3D12_RESOURCE_FLAG_NONE), D3D12_RESOURCE_STATE_COPY_DEST, None, &mut buf), "readback")?;
            let buf = buf.unwrap();
            self.list.ResourceBarrier(&[transition(tex, state, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
            let dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&buf),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp },
            };
            let src = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(tex),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            self.list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
            self.list.ResourceBarrier(&[transition(tex, D3D12_RESOURCE_STATE_COPY_SOURCE, state)]);
            self.run()?;
            let mut p = std::ptr::null_mut();
            hr(buf.Map(0, None, Some(&mut p)), "map readback")?;
            let data = std::slice::from_raw_parts(p as *const u8, total as usize).to_vec();
            buf.Unmap(0, None);
            Ok((fp.Footprint.RowPitch as usize, data))
        }
    }
}

// ---------------------------------------------------------------------------------------
// Root signature and descriptor heap for heap-mode shaders

/// RDNA2 sampler descriptor -> D3D12 static sampler (address modes, filter, compare). The
/// field positions follow the public GFX10 S# layout; only the common fields are mapped.
fn static_sampler(reg: u32, dw: [u32; 4], cmp: bool) -> D3D12_STATIC_SAMPLER_DESC {
    let addr = |v: u32| match v & 7 {
        0 => D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        1 => D3D12_TEXTURE_ADDRESS_MODE_MIRROR,
        2 | 3 => D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        5 => D3D12_TEXTURE_ADDRESS_MODE_MIRROR_ONCE,
        _ => D3D12_TEXTURE_ADDRESS_MODE_BORDER,
    };
    let point = (dw[2] >> 20) & 3 == 0 && (dw[2] >> 22) & 3 == 0;
    let filter = match (cmp, point) {
        (false, false) => D3D12_FILTER_MIN_MAG_MIP_LINEAR,
        (false, true) => D3D12_FILTER_MIN_MAG_MIP_POINT,
        (true, false) => D3D12_FILTER_COMPARISON_MIN_MAG_LINEAR_MIP_POINT,
        (true, true) => D3D12_FILTER_COMPARISON_MIN_MAG_MIP_POINT,
    };
    D3D12_STATIC_SAMPLER_DESC {
        Filter: filter,
        AddressU: addr(dw[0]),
        AddressV: addr(dw[0] >> 3),
        AddressW: addr(dw[0] >> 6),
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC((((dw[0] >> 12) & 7) + 1) as i32),
        MaxLOD: f32::MAX,
        ShaderRegister: reg,
        RegisterSpace: 98,
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        ..Default::default()
    }
}

fn root_signature(dev: &ID3D12Device, samplers: &[([u32; 4], bool)], graphics: bool) -> R<ID3D12RootSignature> {
    unsafe {
        let params = [D3D12_ROOT_PARAMETER1 {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
            Anonymous: D3D12_ROOT_PARAMETER1_0 { Descriptor: D3D12_ROOT_DESCRIPTOR1 { ShaderRegister: 0, RegisterSpace: 100, Flags: D3D12_ROOT_DESCRIPTOR_FLAG_NONE } },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        }];
        let ss: Vec<D3D12_STATIC_SAMPLER_DESC> = samplers.iter().enumerate().map(|(i, (dw, cmp))| static_sampler(i as u32, *dw, *cmp)).collect();
        let mut flags = D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED | D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED;
        if graphics {
            flags |= D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT;
        }
        let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
            Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
            Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
                Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                    NumParameters: params.len() as u32,
                    pParameters: params.as_ptr(),
                    NumStaticSamplers: ss.len() as u32,
                    pStaticSamplers: if ss.is_empty() { std::ptr::null() } else { ss.as_ptr() },
                    Flags: flags,
                },
            },
        };
        let mut blob = None;
        let mut err = None;
        if let Err(e) = D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut err)) {
            let msg = err.map(|b: ID3DBlob| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned()).unwrap_or_default();
            return Err(format!("root signature: {e} {msg}"));
        }
        let blob = blob.unwrap();
        let bytes = std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
        hr(dev.CreateRootSignature(0, bytes), "CreateRootSignature")
    }
}

/// Buffer contents: repeated 4x4 identity matrices (`identity`) or every float 1.0 (`ones`).
fn fill_pattern(bytes: usize, kind: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(bytes);
    for i in 0..bytes / 4 {
        let k = i % 16;
        let f: f32 = if kind == "ones" || k % 5 == 0 { 1.0 } else { 0.0 };
        v.extend_from_slice(&f.to_bits().to_le_bytes());
    }
    v
}

struct Heaps {
    res: ID3D12DescriptorHeap,
    samp: ID3D12DescriptorHeap,
    user: ID3D12Resource,
    /// UAV images written by the shaders, for readback.
    images: Vec<(String, ID3D12Resource)>,
}

fn kind_format(kind: &str) -> DXGI_FORMAT {
    if kind.contains("<uint") {
        DXGI_FORMAT_R32G32B32A32_UINT
    } else if kind.contains("<int") {
        DXGI_FORMAT_R32G32B32A32_SINT
    } else {
        DXGI_FORMAT_R32G32B32A32_FLOAT
    }
}

fn build_heaps(gpu: &mut Gpu, stages: &[&Stage], fill: &str) -> R<Heaps> {
    unsafe {
        let dev = gpu.dev.clone();
        let res: ID3D12DescriptorHeap = hr(dev.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, NumDescriptors: HEAP, Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, NodeMask: 0 }), "resource heap")?;
        let samp: ID3D12DescriptorHeap = hr(dev.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, NumDescriptors: HEAP, Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, NodeMask: 0 }), "sampler heap")?;
        let rinc = dev.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
        let sinc = dev.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER) as usize;
        let rbase = res.GetCPUDescriptorHandleForHeapStart();
        let sbase = samp.GetCPUDescriptorHandleForHeapStart();
        let rh = |i: u32| D3D12_CPU_DESCRIPTOR_HANDLE { ptr: rbase.ptr + (i as usize % HEAP as usize) * rinc };
        let sh = |i: u32| D3D12_CPU_DESCRIPTOR_HANDLE { ptr: sbase.ptr + (i as usize % HEAP as usize) * sinc };

        // Shared resources.
        let cb = gpu.upload(&fill_pattern(65536, fill))?;
        let data = gpu.upload(&fill_pattern(1 << 20, fill))?;
        let checker: Vec<u8> = (0..64 * 64).flat_map(|i| {
            let (x, y) = (i % 64, i / 64);
            let c = if ((x / 8) + (y / 8)) % 2 == 0 { [230u8, 200, 160, 255] } else { [60u8, 90, 140, 255] };
            c
        }).collect();
        let tex2d = gpu.texture(D3D12_RESOURCE_DIMENSION_TEXTURE2D, 64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST, None)?;
        {
            // Upload the checkerboard.
            let up = gpu.upload(&{
                let mut v = vec![0u8; 256 * 64];
                for y in 0..64 {
                    v[y * 256..y * 256 + 256].copy_from_slice(&checker[y * 256..y * 256 + 256]);
                }
                v
            })?;
            let fp = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT { Format: DXGI_FORMAT_R8G8B8A8_UNORM, Width: 64, Height: 64, Depth: 1, RowPitch: 256 },
            };
            let dst = D3D12_TEXTURE_COPY_LOCATION { pResource: std::mem::transmute_copy(&tex2d), Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX, Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 } };
            let src = D3D12_TEXTURE_COPY_LOCATION { pResource: std::mem::transmute_copy(&up), Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT, Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp } };
            gpu.list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
            gpu.list.ResourceBarrier(&[transition(&tex2d, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_ALL_SHADER_RESOURCE)]);
        }
        let tex_arr = gpu.texture(D3D12_RESOURCE_DIMENSION_TEXTURE2D, 16, 16, 6, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON, None)?;
        let tex3d = gpu.texture(D3D12_RESOURCE_DIMENSION_TEXTURE3D, 16, 16, 16, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON, None)?;

        let srv_tex = |i: u32, kind: &str| {
            let (dim, res) = if kind.starts_with("Texture3D") {
                (D3D12_SRV_DIMENSION_TEXTURE3D, &tex3d)
            } else if kind.starts_with("TextureCube") {
                (D3D12_SRV_DIMENSION_TEXTURECUBE, &tex_arr)
            } else if kind.starts_with("Texture2DArray") {
                (D3D12_SRV_DIMENSION_TEXTURE2DARRAY, &tex_arr)
            } else {
                (D3D12_SRV_DIMENSION_TEXTURE2D, &tex2d)
            };
            let mut d = D3D12_SHADER_RESOURCE_VIEW_DESC { Format: DXGI_FORMAT_R8G8B8A8_UNORM, ViewDimension: dim, Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING, ..Default::default() };
            match dim {
                D3D12_SRV_DIMENSION_TEXTURE3D => d.Anonymous.Texture3D = D3D12_TEX3D_SRV { MipLevels: 1, ..Default::default() },
                D3D12_SRV_DIMENSION_TEXTURECUBE => d.Anonymous.TextureCube = D3D12_TEXCUBE_SRV { MipLevels: 1, ..Default::default() },
                D3D12_SRV_DIMENSION_TEXTURE2DARRAY => d.Anonymous.Texture2DArray = D3D12_TEX2D_ARRAY_SRV { MipLevels: 1, ArraySize: 6, ..Default::default() },
                _ => d.Anonymous.Texture2D = D3D12_TEX2D_SRV { MipLevels: 1, ..Default::default() },
            }
            if kind.contains("<uint") || kind.contains("<int") {
                // Integer textures: a null view of the right format class.
                d.Format = kind_format(kind);
                dev.CreateShaderResourceView(None, Some(&d), rh(i));
            } else {
                dev.CreateShaderResourceView(res, Some(&d), rh(i));
            }
        };
        // Fill every slot with a 2D texture view first (the most common kind), and every
        // sampler slot with a linear sampler; reported bindings overwrite them below.
        for i in 0..HEAP {
            srv_tex(i, "Texture2D<float4>");
            dev.CreateSampler(&D3D12_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_WRAP, AddressV: D3D12_TEXTURE_ADDRESS_MODE_WRAP, AddressW: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
                MaxAnisotropy: 1, ComparisonFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL, MaxLOD: f32::MAX, ..Default::default()
            }, sh(i));
        }
        // Table bases: each entry SGPR used as a base gets its own region.
        let mut user = [0u32; 128];
        let mut sgprs: Vec<u8> = stages.iter().flat_map(|s| s.t.heap_bindings.iter().filter_map(|b| b.sgpr)).collect();
        sgprs.sort();
        sgprs.dedup();
        for (k, s) in sgprs.iter().enumerate() {
            user[*s as usize] = (k as u32 * REGION) * 32;
        }
        let mut images = Vec::new();
        for st in stages {
            for b in &st.t.heap_bindings {
                let Some(s) = b.sgpr else { continue };
                let base = user[s as usize];
                let (ri, si) = ((base + b.offset) >> 5, (base + b.offset) >> 4);
                let k = b.kind.as_str();
                if k.starts_with("ConstantBuffer") {
                    dev.CreateConstantBufferView(Some(&D3D12_CONSTANT_BUFFER_VIEW_DESC { BufferLocation: cb.GetGPUVirtualAddress(), SizeInBytes: 65536 }), rh(ri));
                } else if k.starts_with("SamplerComparisonState") {
                    dev.CreateSampler(&D3D12_SAMPLER_DESC { Filter: D3D12_FILTER_COMPARISON_MIN_MAG_LINEAR_MIP_POINT, AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP, AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP, AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP, MaxAnisotropy: 1, ComparisonFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL, MaxLOD: f32::MAX, ..Default::default() }, sh(si));
                } else if k.starts_with("SamplerState") {
                    // the default sampler already sits there
                } else if k.starts_with("Texture") {
                    srv_tex(ri, k);
                } else if k.starts_with("StructuredBuffer") || k.starts_with("Buffer") {
                    let structured = k.starts_with("Structured");
                    let d = D3D12_SHADER_RESOURCE_VIEW_DESC {
                        Format: if structured { DXGI_FORMAT_UNKNOWN } else { kind_format(k) },
                        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
                        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 { Buffer: D3D12_BUFFER_SRV { FirstElement: 0, NumElements: (1 << 20) / 16, StructureByteStride: if structured { 16 } else { 0 }, Flags: D3D12_BUFFER_SRV_FLAG_NONE } },
                    };
                    dev.CreateShaderResourceView(&data, Some(&d), rh(ri));
                } else if k.starts_with("RWStructuredBuffer") || k.starts_with("RWBuffer") {
                    let structured = k.starts_with("RWStructured");
                    let buf = gpu.default_buffer(1 << 20, true)?;
                    let d = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                        Format: if structured { DXGI_FORMAT_UNKNOWN } else if k.contains("<uint>") { DXGI_FORMAT_R32_UINT } else { kind_format(k) },
                        ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 { Buffer: D3D12_BUFFER_UAV { FirstElement: 0, NumElements: (1 << 20) / 16, StructureByteStride: if structured { 16 } else { 0 }, ..Default::default() } },
                    };
                    dev.CreateUnorderedAccessView(&buf, None, Some(&d), rh(ri));
                } else if k.starts_with("RWTexture2D<") {
                    let fmt = if k.contains("RWTexture2D<uint>") { DXGI_FORMAT_R32_UINT } else { kind_format(k) };
                    let img = gpu.texture(D3D12_RESOURCE_DIMENSION_TEXTURE2D, 256, 256, 1, fmt, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, None)?;
                    let d = D3D12_UNORDERED_ACCESS_VIEW_DESC { Format: fmt, ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D, ..Default::default() };
                    dev.CreateUnorderedAccessView(&img, None, Some(&d), rh(ri));
                    images.push((format!("uav_s{s}_{:#x}", b.offset), img));
                } else if k.starts_with("RWTexture") {
                    let fmt = if k.contains("<uint>") { DXGI_FORMAT_R32_UINT } else { kind_format(k) };
                    let three = k.starts_with("RWTexture3D");
                    let img = gpu.texture(if three { D3D12_RESOURCE_DIMENSION_TEXTURE3D } else { D3D12_RESOURCE_DIMENSION_TEXTURE2D }, 64, 64, 8, fmt,
                        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, None)?;
                    let mut d = D3D12_UNORDERED_ACCESS_VIEW_DESC { Format: fmt, ..Default::default() };
                    if three {
                        d.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE3D;
                        d.Anonymous.Texture3D = D3D12_TEX3D_UAV { MipSlice: 0, FirstWSlice: 0, WSize: 8 };
                    } else {
                        d.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE2DARRAY;
                        d.Anonymous.Texture2DArray = D3D12_TEX2D_ARRAY_UAV { ArraySize: 8, ..Default::default() };
                    }
                    dev.CreateUnorderedAccessView(&img, None, Some(&d), rh(ri));
                } else {
                    println!("  note: binding kind {k} not provided");
                }
            }
        }
        let ub: Vec<u8> = user.iter().flat_map(|v| v.to_le_bytes()).collect();
        let user_buf = gpu.upload(&ub)?;
        for (s, v) in user.iter().enumerate() {
            if *v != 0 || sgprs.contains(&(s as u8)) {
                println!("  s{s} = table base, heap index {}", v / 32);
            }
        }
        Ok(Heaps { res, samp, user: user_buf, images })
    }
}

// ---------------------------------------------------------------------------------------
// Output

fn save_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> R<()> {
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgba).map_err(|e| e.to_string())
}

/// Convert a read-back image to 8-bit RGBA and print statistics.
fn image(name: &str, out: &Path, w: u32, h: u32, fmt: DXGI_FORMAT, pitch: usize, data: &[u8]) -> R<()> {
    let (bpp, float, half) = match fmt {
        DXGI_FORMAT_R16G16B16A16_FLOAT => (8, true, true),
        DXGI_FORMAT_R32G32B32A32_FLOAT => (16, true, false),
        DXGI_FORMAT_R32G32B32A32_UINT | DXGI_FORMAT_R32G32B32A32_SINT => (16, false, false),
        DXGI_FORMAT_R32_UINT => (4, false, false),
        _ => return Err(format!("{name}: format {fmt:?} not decoded")),
    };
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let (mut nan, mut inf, mut covered, mut mx) = (0usize, 0usize, 0usize, 0f32);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = &data[y * pitch + x * bpp..y * pitch + x * bpp + bpp];
            let mut v = [0f32; 4];
            for c in 0..(bpp / if half { 2 } else { 4 }).min(4) {
                v[c] = if half {
                    let hb = u16::from_le_bytes([p[c * 2], p[c * 2 + 1]]);
                    half_to_f32(hb)
                } else {
                    let u = u32::from_le_bytes(p[c * 4..c * 4 + 4].try_into().unwrap());
                    if float { f32::from_bits(u) } else { (u % 256) as f32 / 255.0 }
                };
            }
            if bpp == 4 {
                v = [v[0], v[0], v[0], 1.0];
            }
            let mut any = false;
            for c in 0..4 {
                if v[c].is_nan() {
                    nan += 1;
                } else if v[c].is_infinite() {
                    inf += 1;
                } else {
                    mx = mx.max(v[c].abs());
                }
                if v[c] != 0.0 {
                    any = true;
                }
            }
            covered += any as usize;
            let o = (y * w as usize + x) * 4;
            for c in 0..3 {
                let t = if v[c].is_finite() { (v[c].max(0.0) / (1.0 + v[c].max(0.0))).powf(1.0 / 2.2) * 1.6 } else { 1.0 };
                rgba[o + c] = (t.clamp(0.0, 1.0) * 255.0) as u8;
            }
            rgba[o + 3] = 255;
        }
    }
    let path = out.join(format!("{name}.png"));
    save_png(&path, w, h, &rgba)?;
    println!("  {name}: {w}x{h} {fmt:?}: {covered} non-zero pixels ({:.1}%), max |v| {mx:.3}, NaN {nan}, Inf {inf} -> {}",
        100.0 * covered as f64 / (w * h) as f64, path.display());
    Ok(())
}

fn half_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) & 1, (h >> 10) & 0x1f, h & 0x3ff);
    let v = if e == 0 { m as f32 * 2f32.powi(-24) } else if e == 31 { if m == 0 { f32::INFINITY } else { f32::NAN } } else { (1.0 + m as f32 / 1024.0) * 2f32.powi(e as i32 - 15) };
    if s == 1 { -v } else { v }
}

// ---------------------------------------------------------------------------------------

struct Args {
    vs: Option<PathBuf>,
    ps: Option<PathBuf>,
    cs: Option<PathBuf>,
    model: String,
    size: u32,
    out: PathBuf,
    groups: [u32; 3],
    fill: String,
}

fn args() -> R<Args> {
    let mut a = Args { vs: None, ps: None, cs: None, model: "sphere".into(), size: 512, out: PathBuf::from("viewer_out"), groups: [8, 8, 1], fill: "identity".into() };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < v.len() {
        let val = v.get(i + 1).cloned().ok_or_else(|| format!("{} needs a value", v[i]))?;
        match v[i].as_str() {
            "--vs" => a.vs = Some(val.into()),
            "--ps" => a.ps = Some(val.into()),
            "--cs" => a.cs = Some(val.into()),
            "--model" => a.model = val,
            "--size" => a.size = val.parse().map_err(|_| "bad --size")?,
            "--out" => a.out = val.into(),
            "--fill" => a.fill = val,
            "--groups" => {
                let g: Vec<u32> = val.split(',').filter_map(|x| x.parse().ok()).collect();
                for k in 0..3 {
                    a.groups[k] = *g.get(k).unwrap_or(&1);
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
        i += 2;
    }
    if a.cs.is_none() && (a.vs.is_none() || a.ps.is_none()) {
        return Err("usage: tombox-viewer --vs <vs.dxbc> --ps <ps.dxbc> [--model sphere|cube|torus|quad] [--size N] [--out DIR]\n       tombox-viewer --cs <cs.dxbc> [--groups X,Y,Z] [--out DIR]".into());
    }
    Ok(a)
}

fn main() {
    match run() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn run() -> R<()> {
    let a = args()?;
    std::fs::create_dir_all(&a.out).map_err(|e| e.to_string())?;
    let mut gpu = Gpu::new()?;
    if let Some(cs) = &a.cs {
        return run_compute(&mut gpu, &a, cs);
    }
    let vs = load(a.vs.as_ref().unwrap())?;
    let ps = load(a.ps.as_ref().unwrap())?;
    if !vs.t.heap || !ps.t.heap {
        return Err("only shaders without a root signature (descriptor-heap mode) are supported by the viewer so far".into());
    }
    let mut samplers = vs.t.static_samplers.clone();
    if !ps.t.static_samplers.is_empty() && !samplers.is_empty() && ps.t.static_samplers != samplers {
        return Err("vertex and pixel shader use different static samplers (not supported yet)".into());
    }
    if samplers.is_empty() {
        samplers = ps.t.static_samplers.clone();
    }
    let root = root_signature(&gpu.dev, &samplers, true)?;
    let heaps = build_heaps(&mut gpu, &[&vs, &ps], &a.fill)?;
    let m = model(&a.model)?;

    // Vertex input: one float4/uint4 per input-signature element, interleaved.
    let inputs: Vec<&SigElement> = vs.input.iter().filter(|e| e.sysval == 0).collect();
    let names: Vec<std::ffi::CString> = inputs.iter().map(|e| std::ffi::CString::new(e.name.clone()).unwrap()).collect();
    let layout: Vec<D3D12_INPUT_ELEMENT_DESC> = inputs.iter().enumerate().map(|(k, e)| D3D12_INPUT_ELEMENT_DESC {
        SemanticName: PCSTR(names[k].as_ptr() as *const u8),
        SemanticIndex: e.index,
        Format: match e.comp_type { 1 => DXGI_FORMAT_R32G32B32A32_UINT, 2 => DXGI_FORMAT_R32G32B32A32_SINT, _ => DXGI_FORMAT_R32G32B32A32_FLOAT },
        InputSlot: 0,
        AlignedByteOffset: 16 * k as u32,
        InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
        InstanceDataStepRate: 0,
    }).collect();
    let stride = 16 * inputs.len().max(1);
    let mut vb = Vec::with_capacity(stride * m.pos.len());
    for v in 0..m.pos.len() {
        if inputs.is_empty() {
            vb.extend_from_slice(&[0u8; 16]);
        }
        for e in &inputs {
            for w in attribute(e, &m, v) {
                vb.extend_from_slice(&w.to_le_bytes());
            }
        }
    }
    println!("model {}: {} vertices, {} triangles; vertex inputs: {}", a.model, m.pos.len(), m.idx.len() / 3,
        inputs.iter().map(|e| format!("{}{}", e.name, e.index)).collect::<Vec<_>>().join(" "));
    let vbuf = gpu.upload(&vb)?;
    let ib: Vec<u8> = m.idx.iter().flat_map(|i| i.to_le_bytes()).collect();
    let ibuf = gpu.upload(&ib)?;

    // Render targets from the pixel shader's outputs.
    let targets: Vec<&SigElement> = ps.output.iter().filter(|e| e.sysval == 64).collect();
    let writes_depth = ps.output.iter().any(|e| matches!(e.sysval, 65 | 67 | 68));
    let rtv_heap: ID3D12DescriptorHeap = unsafe { hr(gpu.dev.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV, NumDescriptors: 8, ..Default::default() }), "rtv heap")? };
    let dsv_heap: ID3D12DescriptorHeap = unsafe { hr(gpu.dev.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_DSV, NumDescriptors: 1, ..Default::default() }), "dsv heap")? };
    let rtv_inc = unsafe { gpu.dev.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) } as usize;
    let mut rts = Vec::new();
    let mut fmts = [DXGI_FORMAT_UNKNOWN; 8];
    for (k, e) in targets.iter().enumerate().take(8) {
        let fmt = match e.comp_type { 1 => DXGI_FORMAT_R32G32B32A32_UINT, 2 => DXGI_FORMAT_R32G32B32A32_SINT, _ => DXGI_FORMAT_R16G16B16A16_FLOAT };
        let clear = D3D12_CLEAR_VALUE { Format: fmt, Anonymous: D3D12_CLEAR_VALUE_0 { Color: [0.0; 4] } };
        let rt = gpu.texture(D3D12_RESOURCE_DIMENSION_TEXTURE2D, a.size as u64, a.size, 1, fmt, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_STATE_RENDER_TARGET, Some(&clear))?;
        let h = D3D12_CPU_DESCRIPTOR_HANDLE { ptr: unsafe { rtv_heap.GetCPUDescriptorHandleForHeapStart() }.ptr + k * rtv_inc };
        unsafe { gpu.dev.CreateRenderTargetView(&rt, None, h) };
        fmts[k] = fmt;
        rts.push((format!("target{}", e.index), rt, fmt, h));
    }
    let dclear = D3D12_CLEAR_VALUE { Format: DXGI_FORMAT_D32_FLOAT, Anonymous: D3D12_CLEAR_VALUE_0 { DepthStencil: D3D12_DEPTH_STENCIL_VALUE { Depth: 1.0, Stencil: 0 } } };
    let depth = gpu.texture(D3D12_RESOURCE_DIMENSION_TEXTURE2D, a.size as u64, a.size, 1, DXGI_FORMAT_D32_FLOAT, D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL, D3D12_RESOURCE_STATE_DEPTH_WRITE, Some(&dclear))?;
    let dsv = unsafe { dsv_heap.GetCPUDescriptorHandleForHeapStart() };
    unsafe { gpu.dev.CreateDepthStencilView(&depth, None, dsv) };

    let mut pso_desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: unsafe { std::mem::transmute_copy(&root) },
        VS: D3D12_SHADER_BYTECODE { pShaderBytecode: vs.dxil.as_ptr() as *const _, BytecodeLength: vs.dxil.len() },
        PS: D3D12_SHADER_BYTECODE { pShaderBytecode: ps.dxil.as_ptr() as *const _, BytecodeLength: ps.dxil.len() },
        SampleMask: u32::MAX,
        RasterizerState: D3D12_RASTERIZER_DESC { FillMode: D3D12_FILL_MODE_SOLID, CullMode: D3D12_CULL_MODE_NONE, DepthClipEnable: TRUE, ..Default::default() },
        DepthStencilState: D3D12_DEPTH_STENCIL_DESC { DepthEnable: TRUE, DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL, DepthFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL, ..Default::default() },
        InputLayout: D3D12_INPUT_LAYOUT_DESC { pInputElementDescs: if layout.is_empty() { std::ptr::null() } else { layout.as_ptr() }, NumElements: layout.len() as u32 },
        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
        NumRenderTargets: rts.len() as u32,
        RTVFormats: fmts,
        DSVFormat: DXGI_FORMAT_D32_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        ..Default::default()
    };
    for k in 0..8 {
        pso_desc.BlendState.RenderTarget[k].RenderTargetWriteMask = 0xf;
    }
    if writes_depth {
        pso_desc.DepthStencilState.DepthFunc = D3D12_COMPARISON_FUNC_ALWAYS;
    }
    let pso: ID3D12PipelineState = unsafe { hr(gpu.dev.CreateGraphicsPipelineState(&pso_desc), "CreateGraphicsPipelineState")? };
    unsafe {
        let l = gpu.list.clone();
        l.SetDescriptorHeaps(&[Some(heaps.res.clone()), Some(heaps.samp.clone())]);
        l.SetGraphicsRootSignature(&root);
        l.SetGraphicsRootConstantBufferView(0, heaps.user.GetGPUVirtualAddress());
        l.SetPipelineState(&pso);
        let handles: Vec<D3D12_CPU_DESCRIPTOR_HANDLE> = rts.iter().map(|r| r.3).collect();
        for h in &handles {
            l.ClearRenderTargetView(*h, &[0.0, 0.0, 0.0, 0.0], None);
        }
        l.ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 1.0, 0, &[]);
        l.OMSetRenderTargets(handles.len() as u32, if handles.is_empty() { None } else { Some(handles.as_ptr()) }, false, Some(&dsv));
        l.RSSetViewports(&[D3D12_VIEWPORT { Width: a.size as f32, Height: a.size as f32, MaxDepth: 1.0, ..Default::default() }]);
        l.RSSetScissorRects(&[RECT { left: 0, top: 0, right: a.size as i32, bottom: a.size as i32 }]);
        l.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        l.IASetVertexBuffers(0, Some(&[D3D12_VERTEX_BUFFER_VIEW { BufferLocation: vbuf.GetGPUVirtualAddress(), SizeInBytes: vb.len() as u32, StrideInBytes: stride as u32 }]));
        l.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW { BufferLocation: ibuf.GetGPUVirtualAddress(), SizeInBytes: ib.len() as u32, Format: DXGI_FORMAT_R32_UINT }));
        l.DrawIndexedInstanced(m.idx.len() as u32, 1, 0, 0, 0);
    }
    gpu.run()?;
    println!("drew {} + {} on a {}:", vs.name, ps.name, a.model);
    for (name, rt, fmt, _) in &rts {
        let (pitch, data) = gpu.readback(rt, D3D12_RESOURCE_STATE_RENDER_TARGET)?;
        image(name, &a.out, a.size, a.size, *fmt, pitch, &data)?;
    }
    for (name, img) in &heaps.images {
        let d = unsafe { img.GetDesc() };
        let (pitch, data) = gpu.readback(img, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
        image(name, &a.out, d.Width as u32, d.Height, d.Format, pitch, &data)?;
    }
    Ok(())
}

fn run_compute(gpu: &mut Gpu, a: &Args, path: &Path) -> R<()> {
    let cs = load(path)?;
    if !cs.t.heap {
        return Err("only shaders without a root signature (descriptor-heap mode) are supported by the viewer so far".into());
    }
    let root = root_signature(&gpu.dev, &cs.t.static_samplers, false)?;
    let heaps = build_heaps(gpu, &[&cs], &a.fill)?;
    let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
        pRootSignature: unsafe { std::mem::transmute_copy(&root) },
        CS: D3D12_SHADER_BYTECODE { pShaderBytecode: cs.dxil.as_ptr() as *const _, BytecodeLength: cs.dxil.len() },
        ..Default::default()
    };
    let pso: ID3D12PipelineState = unsafe { hr(gpu.dev.CreateComputePipelineState(&desc), "CreateComputePipelineState")? };
    unsafe {
        let l = gpu.list.clone();
        l.SetDescriptorHeaps(&[Some(heaps.res.clone()), Some(heaps.samp.clone())]);
        l.SetComputeRootSignature(&root);
        l.SetComputeRootConstantBufferView(0, heaps.user.GetGPUVirtualAddress());
        l.SetPipelineState(&pso);
        l.Dispatch(a.groups[0], a.groups[1], a.groups[2]);
    }
    gpu.run()?;
    println!("dispatched {} x {:?}:", cs.name, a.groups);
    if heaps.images.is_empty() {
        println!("  (no 2D image outputs to show)");
    }
    for (name, img) in &heaps.images {
        let d = unsafe { img.GetDesc() };
        let (pitch, data) = gpu.readback(img, D3D12_RESOURCE_STATE_UNORDERED_ACCESS)?;
        image(name, &a.out, d.Width as u32, d.Height, d.Format, pitch, &data)?;
    }
    Ok(())
}
