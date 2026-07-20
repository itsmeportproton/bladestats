//! Рендер оверлея: D3D11 поверх swapchain композиции, содержимое — из `bs-render`.
//!
//! Путь картинки: D3D11-устройство → swapchain, созданный под композицию (premultiplied
//! alpha) → визуал DirectComposition, привязанный к окну. Никакого `UpdateLayeredWindow`,
//! никакой пересылки битмапа через CPU — альфа считается на видеокарте.
//!
//! Шейдер один и тот же для текста и для заливок: сплошные прямоугольники берут
//! непрозрачный тексель атласа, поэтому весь оверлей рисуется одним вызовом.

use std::ffi::c_void;

use anyhow::{Context, Result, anyhow};
use bs_render::{DrawList, GlyphAtlas, Vertex};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, PCSTR, s};

/// Исходник шейдера. Держим здесь, а не в отдельном файле: он короткий, и рядом с кодом,
/// который заводит буферы, его проще держать согласованным.
const SHADER: &str = r#"
cbuffer Params : register(b0) {
    float2 inv_viewport;   // 1 / (ширина, высота) в пикселях
    float2 _padding;
};

struct VSIn  { float2 pos : POSITION; float2 uv : TEXCOORD0; float4 col : COLOR0; };
struct VSOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; float4 col : COLOR0; };

VSOut vs_main(VSIn i) {
    VSOut o;
    // Пиксели с началом в левом верхнем углу переводятся в NDC.
    o.pos = float4(i.pos.x * inv_viewport.x * 2.0 - 1.0,
                   1.0 - i.pos.y * inv_viewport.y * 2.0, 0.0, 1.0);
    o.uv = i.uv;
    o.col = i.col;
    return o;
}

Texture2D<float> atlas : register(t0);
SamplerState atlas_sampler : register(s0);

float4 ps_main(VSOut i) : SV_TARGET {
    // В атласе лежит только покрытие; цвет приходит из вершины уже premultiplied,
    // поэтому домножение всей четвёрки сохраняет premultiplied-инвариант.
    return i.col * atlas.Sample(atlas_sampler, i.uv);
}
"#;

#[repr(C)]
struct Params {
    inv_viewport: [f32; 2],
    _padding: [f32; 2],
}

pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,

    // Композиция должна пережить рендерер: пока эти объекты живы, картинка на экране.
    _dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,

    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    blend: ID3D11BlendState,
    sampler: ID3D11SamplerState,
    params: ID3D11Buffer,
    _atlas_texture: ID3D11Texture2D,
    atlas_srv: ID3D11ShaderResourceView,

    vertex_buffer: Option<ID3D11Buffer>,
    index_buffer: Option<ID3D11Buffer>,
    vertex_capacity: usize,
    index_capacity: usize,

    width: u32,
    height: u32,
}

impl Renderer {
    pub fn new(hwnd: HWND, atlas: &GlyphAtlas, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                // BGRA_SUPPORT нужен для взаимодействия с композицией.
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice")?;
            let device = device.ok_or_else(|| anyhow!("D3D11 не вернул устройство"))?;
            let context = context.ok_or_else(|| anyhow!("D3D11 не вернул контекст"))?;

            let dxgi_device: IDXGIDevice = device.cast()?;
            let adapter = dxgi_device.GetAdapter().context("GetAdapter")?;
            let factory: IDXGIFactory2 = adapter.GetParent().context("IDXGIFactory2")?;

            let swapchain = factory
                .CreateSwapChainForComposition(
                    &device,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: width.max(1),
                        Height: height.max(1),
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        // Композиция требует flip-модели и premultiplied-альфы.
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                        ..Default::default()
                    },
                    None,
                )
                .context("CreateSwapChainForComposition")?;

            // Дерево композиции: устройство → цель, привязанная к окну → визуал с swapchain.
            let dcomp_device: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi_device).context("DCompositionCreateDevice")?;

            let dcomp_target = dcomp_device
                .CreateTargetForHwnd(hwnd, true)
                .context("CreateTargetForHwnd")?;
            let dcomp_visual = dcomp_device.CreateVisual().context("CreateVisual")?;
            dcomp_visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&dcomp_visual)?;
            dcomp_device.Commit()?;

            let (vs, vs_blob) = compile_vertex_shader(&device)?;
            let ps = compile_pixel_shader(&device)?;

            let layout = create_input_layout(&device, &vs_blob)?;
            let blend = create_blend_state(&device)?;
            let sampler = create_sampler(&device)?;
            let params = create_params_buffer(&device)?;
            let (atlas_texture, atlas_srv) = upload_atlas(&device, atlas)?;

            let mut renderer = Self {
                device,
                context,
                swapchain,
                rtv: None,
                _dcomp_device: dcomp_device,
                _dcomp_target: dcomp_target,
                _dcomp_visual: dcomp_visual,
                vs,
                ps,
                layout,
                blend,
                sampler,
                params,
                _atlas_texture: atlas_texture,
                atlas_srv,
                vertex_buffer: None,
                index_buffer: None,
                vertex_capacity: 0,
                index_capacity: 0,
                width: width.max(1),
                height: height.max(1),
            };
            renderer.create_rtv()?;
            Ok(renderer)
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        unsafe {
            // Все ссылки на буферы swapchain должны быть отпущены до ResizeBuffers.
            self.rtv = None;
            self.context.OMSetRenderTargets(None, None);
            self.swapchain
                .ResizeBuffers(
                    0,
                    width,
                    height,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
                .context("ResizeBuffers")?;
        }
        self.width = width;
        self.height = height;
        self.create_rtv()
    }

    fn create_rtv(&mut self) -> Result<()> {
        unsafe {
            let back: ID3D11Texture2D = self.swapchain.GetBuffer(0).context("GetBuffer")?;
            let mut rtv = None;
            self.device
                .CreateRenderTargetView(&back, None, Some(&mut rtv))
                .context("CreateRenderTargetView")?;
            self.rtv = rtv;
        }
        Ok(())
    }

    /// Рисует список отрисовки и показывает кадр.
    pub fn render(&mut self, list: &DrawList) -> Result<()> {
        unsafe {
            let rtv = self
                .rtv
                .clone()
                .ok_or_else(|| anyhow!("нет цели отрисовки"))?;

            // Прозрачная очистка: всё, что мы не нарисовали, показывает игру под нами.
            self.context
                .ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 0.0]);
            self.context.OMSetRenderTargets(Some(&[Some(rtv)]), None);

            if list.is_empty() {
                self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
                return Ok(());
            }

            self.upload_geometry(list)?;

            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.width as f32,
                Height: self.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));

            let params = Params {
                inv_viewport: [1.0 / self.width as f32, 1.0 / self.height as f32],
                _padding: [0.0, 0.0],
            };
            self.context.UpdateSubresource(
                &self.params,
                0,
                None,
                &params as *const _ as *const c_void,
                0,
                0,
            );

            self.context.IASetInputLayout(&self.layout);
            self.context
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetVertexBuffers(
                0,
                1,
                Some(&self.vertex_buffer),
                Some(&(size_of::<Vertex>() as u32)),
                Some(&0),
            );
            self.context
                .IASetIndexBuffer(self.index_buffer.as_ref(), DXGI_FORMAT_R32_UINT, 0);

            self.context.VSSetShader(&self.vs, None);
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            self.context.PSSetShader(&self.ps, None);
            self.context
                .PSSetShaderResources(0, Some(&[Some(self.atlas_srv.clone())]));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context
                .OMSetBlendState(&self.blend, Some(&[0.0; 4]), 0xFFFF_FFFF);

            self.context.DrawIndexed(list.indices.len() as u32, 0, 0);

            // Интервал 1: оверлею незачем обгонять развёртку, а лишние кадры это лишние ватты.
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Заливает вершины и индексы, при нехватке места пересоздавая буферы с запасом.
    fn upload_geometry(&mut self, list: &DrawList) -> Result<()> {
        unsafe {
            if self.vertex_capacity < list.vertices.len() {
                let capacity = list.vertices.len().next_power_of_two().max(1024);
                self.vertex_buffer = Some(create_dynamic_buffer(
                    &self.device,
                    (capacity * size_of::<Vertex>()) as u32,
                    D3D11_BIND_VERTEX_BUFFER,
                )?);
                self.vertex_capacity = capacity;
            }
            if self.index_capacity < list.indices.len() {
                let capacity = list.indices.len().next_power_of_two().max(1024);
                self.index_buffer = Some(create_dynamic_buffer(
                    &self.device,
                    (capacity * size_of::<u32>()) as u32,
                    D3D11_BIND_INDEX_BUFFER,
                )?);
                self.index_capacity = capacity;
            }

            self.write_buffer(self.vertex_buffer.clone().unwrap(), &list.vertices)?;
            self.write_buffer(self.index_buffer.clone().unwrap(), &list.indices)?;
        }
        Ok(())
    }

    unsafe fn write_buffer<T>(&self, buffer: ID3D11Buffer, data: &[T]) -> Result<()> {
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                .context("Map")?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.pData as *mut T, data.len());
            self.context.Unmap(&buffer, 0);
        }
        Ok(())
    }
}

fn compile(
    source: &str,
    entry: PCSTR,
    target: PCSTR,
) -> Result<windows::Win32::Graphics::Direct3D::ID3DBlob> {
    unsafe {
        let mut code = None;
        let mut errors = None;
        let result = D3DCompile(
            source.as_ptr() as *const c_void,
            source.len(),
            s!("bladestats.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        );

        if result.is_err() {
            // Сообщение компилятора куда полезнее HRESULT, поэтому вытаскиваем его.
            let message = errors
                .as_ref()
                .map(|e| {
                    let bytes = std::slice::from_raw_parts(
                        e.GetBufferPointer() as *const u8,
                        e.GetBufferSize(),
                    );
                    String::from_utf8_lossy(bytes).into_owned()
                })
                .unwrap_or_else(|| "компилятор шейдеров не объяснил причину".into());
            return Err(anyhow!("не скомпилировался шейдер: {message}"));
        }
        code.ok_or_else(|| anyhow!("компилятор шейдеров не вернул код"))
    }
}

fn compile_vertex_shader(
    device: &ID3D11Device,
) -> Result<(
    ID3D11VertexShader,
    windows::Win32::Graphics::Direct3D::ID3DBlob,
)> {
    unsafe {
        let blob = compile(SHADER, s!("vs_main"), s!("vs_5_0"))?;
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
        let mut shader = None;
        device
            .CreateVertexShader(bytes, None, Some(&mut shader))
            .context("CreateVertexShader")?;
        Ok((shader.unwrap(), blob))
    }
}

fn compile_pixel_shader(device: &ID3D11Device) -> Result<ID3D11PixelShader> {
    unsafe {
        let blob = compile(SHADER, s!("ps_main"), s!("ps_5_0"))?;
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize());
        let mut shader = None;
        device
            .CreatePixelShader(bytes, None, Some(&mut shader))
            .context("CreatePixelShader")?;
        Ok(shader.unwrap())
    }
}

fn create_input_layout(
    device: &ID3D11Device,
    vs_blob: &windows::Win32::Graphics::Direct3D::ID3DBlob,
) -> Result<ID3D11InputLayout> {
    unsafe {
        let desc = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("TEXCOORD"),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                AlignedByteOffset: 8,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("COLOR"),
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                AlignedByteOffset: 16,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                ..Default::default()
            },
        ];
        let bytes = std::slice::from_raw_parts(
            vs_blob.GetBufferPointer() as *const u8,
            vs_blob.GetBufferSize(),
        );
        let mut layout = None;
        device
            .CreateInputLayout(&desc, bytes, Some(&mut layout))
            .context("CreateInputLayout")?;
        Ok(layout.unwrap())
    }
}

/// Смешивание для premultiplied alpha: цвет уже умножен на альфу, поэтому источник берётся
/// как есть, а приёмник гасится на `1 - alpha`.
fn create_blend_state(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    unsafe {
        let mut desc = D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend = None;
        device
            .CreateBlendState(&desc, Some(&mut blend))
            .context("CreateBlendState")?;
        Ok(blend.unwrap())
    }
}

/// Точечная фильтрация: атлас растеризован ровно под нужный кегль и никогда не масштабируется,
/// а билинейная выборка только размыла бы и без того мелкий текст.
fn create_sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState> {
    unsafe {
        let desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler = None;
        device
            .CreateSamplerState(&desc, Some(&mut sampler))
            .context("CreateSamplerState")?;
        Ok(sampler.unwrap())
    }
}

fn create_params_buffer(device: &ID3D11Device) -> Result<ID3D11Buffer> {
    unsafe {
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: size_of::<Params>() as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let mut buffer = None;
        device
            .CreateBuffer(&desc, None, Some(&mut buffer))
            .context("CreateBuffer(params)")?;
        Ok(buffer.unwrap())
    }
}

fn create_dynamic_buffer(
    device: &ID3D11Device,
    bytes: u32,
    bind: D3D11_BIND_FLAG,
) -> Result<ID3D11Buffer> {
    unsafe {
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: bytes,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let mut buffer = None;
        device
            .CreateBuffer(&desc, None, Some(&mut buffer))
            .context("CreateBuffer(dynamic)")?;
        Ok(buffer.unwrap())
    }
}

/// Заливает атлас в неизменяемую текстуру R8: в ней лежит только покрытие, цвет берётся
/// из вершин, поэтому одного канала достаточно.
fn upload_atlas(
    device: &ID3D11Device,
    atlas: &GlyphAtlas,
) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: atlas.width,
            Height: atlas.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: atlas.pixels.as_ptr() as *const c_void,
            SysMemPitch: atlas.width,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        device
            .CreateTexture2D(&desc, Some(&data), Some(&mut texture))
            .context("CreateTexture2D(atlas)")?;
        let texture = texture.unwrap();

        let mut srv = None;
        device
            .CreateShaderResourceView(&texture, None, Some(&mut srv))
            .context("CreateShaderResourceView(atlas)")?;
        Ok((texture, srv.unwrap()))
    }
}
