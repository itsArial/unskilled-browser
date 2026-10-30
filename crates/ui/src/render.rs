use crate::{
    Color, Scene,
    scene::{Item, Quad},
};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

/// Draws a [`Scene`]: instanced SDF quads, interleaved with textured layers in draw order.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    atlas: wgpu::Texture,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    capacity: usize,
    image_instances: wgpu::Buffer,
    image_capacity: usize,
}

impl Renderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ui globals"),
            contents: bytemuck::bytes_of(&Globals { viewport: [1.0, 1.0], _pad: [0.0; 2] }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui globals layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // Glyph atlas: single-channel coverage, sampled 1:1 (glyphs are pixel aligned).
        let atlas_size = crate::text::Typesetter::atlas_size();
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui glyph atlas"),
            size: wgpu::Extent3d { width: atlas_size, height: atlas_size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui glyph sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui globals"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&atlas_sampler) },
            ],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4
        ];

        let make_pipeline = |label: &str, entry: &str, layouts: &[Option<&wgpu::BindGroupLayout>]| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Quad>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &ATTRIBUTES,
                    })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline("ui quads", "fs_main", &[Some(&bind_group_layout)]);
        let image_pipeline =
            make_pipeline("ui images", "fs_image", &[Some(&bind_group_layout), Some(&texture_layout)]);

        // Pages are drawn 1:1 in physical pixels; linear only matters mid-animation.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui page sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let capacity = 1024;
        Self {
            pipeline,
            image_pipeline,
            globals,
            bind_group,
            texture_layout,
            atlas,
            sampler,
            instances: Self::instance_buffer(device, capacity),
            capacity,
            image_instances: Self::instance_buffer(device, 8),
            image_capacity: 8,
        }
    }

    fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui instances"),
            size: (capacity * std::mem::size_of::<Quad>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Clears `target` to `background` and draws `scene` over it.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        viewport: (u32, u32),
        background: Color,
        scene: &mut Scene,
    ) {
        scene.finish();
        let atlas = &mut scene.text.atlas;
        if atlas.dirty {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &atlas.data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(atlas.size), rows_per_image: Some(atlas.size) },
                wgpu::Extent3d { width: atlas.size, height: atlas.size, depth_or_array_layers: 1 },
            );
            atlas.dirty = false;
        }
        let images: Vec<Quad> = scene
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Image { quad, .. } => Some(*quad),
                Item::Quads(_) => None,
            })
            .collect();
        if images.len() > self.image_capacity {
            self.image_capacity = images.len().next_power_of_two();
            self.image_instances = Self::instance_buffer(device, self.image_capacity);
        }
        if !images.is_empty() {
            queue.write_buffer(&self.image_instances, 0, bytemuck::cast_slice(&images));
        }
        if scene.quads.len() > self.capacity {
            self.capacity = scene.quads.len().next_power_of_two();
            self.instances = Self::instance_buffer(device, self.capacity);
        }
        queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&Globals { viewport: [viewport.0 as f32, viewport.1 as f32], _pad: [0.0; 2] }),
        );
        if !scene.quads.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&scene.quads));
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ui") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(background.to_wgpu()),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            let mut image_index = 0u32;
            for item in &scene.items {
                match item {
                    Item::Quads(range) => {
                        pass.set_pipeline(&self.pipeline);
                        pass.set_vertex_buffer(0, self.instances.slice(..));
                        pass.draw(0..4, range.clone());
                    }
                    Item::Image { texture, .. } => {
                        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("ui page"),
                            layout: &self.texture_layout,
                            entries: &[
                                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                            ],
                        });
                        pass.set_pipeline(&self.image_pipeline);
                        pass.set_bind_group(1, &group, &[]);
                        pass.set_vertex_buffer(0, self.image_instances.slice(..));
                        pass.draw(0..4, image_index..image_index + 1);
                        image_index += 1;
                    }
                }
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}
