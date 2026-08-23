use std::sync::Arc;

use anyhow::{Context, Result};
use egui::ViewportId;
use ruffle_render_wgpu::backend::{create_wgpu_instance, request_adapter_and_device};
use ruffle_render_wgpu::descriptors::Descriptors;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::GRAPHICS_BACKENDS;
use crate::log::{LogTab, SharedLogState};
use crate::log_ui;
use crate::theme::{self, LOG_BACKGROUND};

const DEFAULT_SIZE: PhysicalSize<u32> = PhysicalSize::new(360, 600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogWindowAction {
    Close,
    TabSelected(LogTab),
}

pub struct LogWindow {
    surface: wgpu::Surface<'static>,
    window: Window,
    surface_config: wgpu::SurfaceConfiguration,
    egui_winit: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    descriptors: Arc<Descriptors>,
    minimized: bool,
}

impl LogWindow {
    pub fn new(event_loop: &ActiveEventLoop) -> Result<Self> {
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("DragonFable - Game Log")
                    .with_inner_size(DEFAULT_SIZE),
            )
            .context("log window creation failed")?;
        let instance = create_wgpu_instance(GRAPHICS_BACKENDS, wgpu::BackendOptions::default());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(&window)?)?
        };
        let (adapter, device, queue) = futures::executor::block_on(request_adapter_and_device(
            GRAPHICS_BACKENDS,
            &instance,
            Some(&surface),
            wgpu::PowerPreference::LowPower,
        ))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));
        let capabilities = surface.get_capabilities(&descriptors.adapter);
        let format = preferred_surface_format(&capabilities.formats)
            .context("log window surface has no supported formats")?;
        let surface_config = surface_configuration(format, window.inner_size());
        surface.configure(&descriptors.device, &surface_config);

        let egui_ctx = egui::Context::default();
        theme::configure(&egui_ctx);
        let mut egui_winit =
            egui_winit::State::new(egui_ctx, ViewportId::ROOT, &window, None, None, None);
        egui_winit.set_max_texture_side(descriptors.limits.max_texture_dimension_2d as usize);
        let egui_renderer = egui_wgpu::Renderer::new(
            &descriptors.device,
            format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );

        Ok(Self {
            surface,
            window,
            surface_config,
            egui_winit,
            egui_renderer,
            descriptors,
            minimized: false,
        })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        logs: &SharedLogState,
    ) -> Option<LogWindowAction> {
        if matches!(event, WindowEvent::CloseRequested) {
            return Some(LogWindowAction::Close);
        }

        let response = self.egui_winit.on_window_event(&self.window, event);
        let action = match event {
            WindowEvent::RedrawRequested if !self.minimized => {
                self.render(logs).map(LogWindowAction::TabSelected)
            }
            WindowEvent::Resized(size) => {
                self.resize(*size);
                None
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.resize(self.window.inner_size());
                None
            }
            _ => None,
        };
        if response.repaint {
            self.window.request_redraw();
        }
        action
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.minimized = size.width == 0 || size.height == 0;
        if self.minimized {
            return;
        }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface
            .configure(&self.descriptors.device, &self.surface_config);
        self.window.request_redraw();
    }

    fn render(&mut self, logs: &SharedLogState) -> Option<LogTab> {
        let surface_texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.resize(self.window.inner_size());
                return None;
            }
            Err(wgpu::SurfaceError::Timeout) => return None,
            Err(error @ (wgpu::SurfaceError::OutOfMemory | wgpu::SurfaceError::Other)) => {
                panic!("wgpu: log surface error: {error:?}");
            }
        };

        let raw_input = self.egui_winit.take_egui_input(&self.window);
        let snapshot = logs.snapshot();
        let mut response = log_ui::LogUiResponse::default();
        let full_output = self.egui_winit.egui_ctx().run(raw_input, |ctx| {
            response = egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(LOG_BACKGROUND))
                .show(ctx, |ui| log_ui::content(ui, &snapshot, false))
                .inner;
        });
        let selected_tab = response.selected_tab();
        response.apply(logs);
        self.egui_winit
            .handle_platform_output(&self.window, full_output.platform_output);

        let clipped_primitives = self
            .egui_winit
            .egui_ctx()
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: self.window.scale_factor() as f32,
        };
        let device = &self.descriptors.device;
        let queue = &self.descriptors.queue;
        let mut encoder = device.create_command_encoder(&Default::default());

        for (id, image_delta) in &full_output.textures_delta.set {
            self.egui_renderer
                .update_texture(device, queue, *id, image_delta);
        }
        let mut command_buffers = self.egui_renderer.update_buffers(
            device,
            queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );
        let surface_view = surface_texture.texture.create_view(&Default::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("log window pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0x1A as f64 / 255.0,
                                g: 0x04 as f64 / 255.0,
                                b: 0x04 as f64 / 255.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            self.egui_renderer
                .render(&mut pass, &clipped_primitives, &screen_descriptor);
        }
        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        command_buffers.push(encoder.finish());
        queue.submit(command_buffers);
        surface_texture.present();
        selected_tab
    }
}

fn preferred_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Bgra8Unorm,
    ]
    .into_iter()
    .find(|format| formats.contains(format))
    .or_else(|| formats.first().copied())
}

fn surface_configuration(
    format: wgpu::TextureFormat,
    size: PhysicalSize<u32>,
) -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::AutoNoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_surface_format_uses_a_non_srgb_egui_format() {
        assert_eq!(
            preferred_surface_format(&[
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ]),
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
    }

    #[test]
    fn preferred_surface_format_falls_back_to_the_first_supported_format() {
        assert_eq!(
            preferred_surface_format(&[wgpu::TextureFormat::Rgba16Float]),
            Some(wgpu::TextureFormat::Rgba16Float)
        );
    }

    #[test]
    fn surface_configuration_is_non_blocking_and_never_has_a_zero_extent() {
        let config =
            surface_configuration(wgpu::TextureFormat::Rgba8Unorm, PhysicalSize::new(0, 0));
        assert_eq!((config.width, config.height), (1, 1));
        assert_eq!(config.present_mode, wgpu::PresentMode::AutoNoVsync);
    }
}
