//! The winit application: window, wgpu + egui setup, screen rendering, and
//! input forwarding to the player.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use egui::ViewportId;
use ruffle_core::events::{
    ImeEvent, MouseButton, MouseInputSource, MouseWheelDelta, PlayerEvent,
};
use ruffle_core::{FloatDuration, Player, ViewportDimensions};
use ruffle_render_wgpu::backend::{
    WgpuRenderBackend, create_wgpu_instance, request_adapter_and_device,
};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, Modifiers, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::config::{self, State};
use crate::input::{winit_input_to_ruffle_key_descriptor, winit_to_ruffle_text_control};
use crate::migration;
use crate::player::{RuffleEvent, build_player, refetch_root_movie};
use crate::ui::{MigratedSource, Screen, initial_screen};

/// The wgpu backend family the app renders with. GL is used (not Vulkan)
/// because the Vulkan backend currently has a memory leak in wgpu.
const GRAPHICS_BACKENDS: wgpu::Backends = wgpu::Backends::GL;
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

fn shutdown_runtime_bound<T>(
    resource: &mut Option<T>,
    runtime: &mut Option<tokio::runtime::Runtime>,
) {
    {
        let _runtime_guard = runtime.as_ref().map(|runtime| runtime.enter());
        drop(resource.take());
    }
    if let Some(runtime) = runtime.take() {
        runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    }
}

pub struct App {
    window: Option<Arc<Window>>,
    event_loop: EventLoopProxy<RuffleEvent>,
    state: State,
    screen: Screen,
    /// Sources detected at startup; re-checked after the disclaimer is accepted
    /// to decide whether the migration setup screen is needed.
    detected_sources: Vec<MigratedSource>,
    // wgpu + egui, constructed once the window exists
    descriptors: Option<Arc<Descriptors>>,
    egui_winit: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    surface: Option<wgpu::Surface<'static>>,
    surface_format: Option<wgpu::TextureFormat>,
    // player state
    player: Option<Arc<Mutex<Player>>>,
    movie_size: Arc<Mutex<Option<(u32, u32)>>>,
    /// The GPU texture the player renders into, sized to the movie's current
    /// on-screen area in physical pixels.
    /// The renderer owns the authoritative target; this mirrors it so the app
    /// can detect viewport changes and re-register the egui texture.
    movie_target: Option<TextureTarget>,
    movie_texture_id: Option<egui::TextureId>,
    movie_viewport_scale_factor: Option<f64>,
    root_error: Arc<Mutex<Option<String>>>,
    font_database: Rc<fontdb::Database>,
    time: Instant,
    last_pointer: PhysicalPosition<f64>,
    modifiers: Modifiers,
    /// Tokio runtime whose context must be entered whenever ruffle futures are
    /// polled or player methods may touch the navigator (ruffle's backend uses
    /// `tokio::spawn` directly, which panics without an entered runtime).
    runtime: Option<tokio::runtime::Runtime>,
}

impl App {
    pub fn new(event_loop: &EventLoop<RuffleEvent>) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()?;
        let _runtime_guard = runtime.enter();
        let event_loop_proxy = event_loop.create_proxy();

        // Creating the window up front (rather than in `new_events(Init)`)
        // keeps the whole app in one place; `EventLoop::create_window` is
        // deprecated in winit 0.30 but fully functional on Windows/X11.
        #[allow(deprecated)]
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("DragonFable")
                        .with_inner_size(PhysicalSize::new(1280, 800)),
                )
                .context("window creation failed")?,
        );

        // wgpu instance + adapter + device (mirrors ruffle_desktop gui/controller.rs:52-118).
        let instance = create_wgpu_instance(GRAPHICS_BACKENDS, wgpu::BackendOptions::default());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(window.as_ref())?)?
        };
        let (adapter, device, queue) =
            futures::executor::block_on(request_adapter_and_device(
                GRAPHICS_BACKENDS,
                &instance,
                Some(&surface),
                wgpu::PowerPreference::HighPerformance,
            ))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let adapter_info = adapter.get_info();
        tracing::info!(
            "Using graphics API {} on {} (type: {:?})",
            adapter_info.backend.to_str(),
            adapter_info.name,
            adapter_info.device_type
        );
        let supported_formats = surface.get_capabilities(&adapter).formats;
        let surface_format = [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ]
        .iter()
        .find(|format| supported_formats.contains(format))
        .copied()
        .unwrap_or_else(|| {
            supported_formats
                .first()
                .copied()
                .expect("at least one surface format must be supported")
        });
        let size = window.inner_size();
        surface.configure(
            &device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: size.width,
                height: size.height,
                present_mode: wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: Vec::new(),
            },
        );

        let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));

        // egui setup (mirrors controller.rs:118-145).
        let egui_ctx = egui::Context::default();
        let mut egui_winit = egui_winit::State::new(
            egui_ctx,
            ViewportId::ROOT,
            window.as_ref(),
            None,
            None,
            None,
        );
        egui_winit.set_max_texture_side(descriptors.limits.max_texture_dimension_2d as usize);
        let egui_renderer = egui_wgpu::Renderer::new(
            &descriptors.device,
            surface_format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );

        // System fonts for the game's device-font text.
        let mut font_database = fontdb::Database::new();
        font_database.load_system_fonts();
        let font_database = Rc::new(font_database);

        // First-boot state + migration scan.
        let state = State::load(&config::config_dir());
        let detected: Vec<MigratedSource> = migration::sources()
            .iter()
            .filter(|source| {
                source
                    .roots
                    .iter()
                    .any(|root| !migration::detect(root, source.include_proxy_host).is_empty())
            })
            .map(Into::into)
            .collect();
        tracing::info!(
            "Detected migration sources: {:?}",
            detected.iter().map(|source| &source.id).collect::<Vec<_>>()
        );
        let screen = initial_screen(&state, &detected);
        let detected_sources = detected;

        let movie_size = Arc::new(Mutex::new(None));
        let root_error = Arc::new(Mutex::new(None));

        let mut app = Self {
            window: Some(window),
            event_loop: event_loop_proxy,
            state,
            screen,
            detected_sources,
            descriptors: Some(descriptors),
            egui_winit: Some(egui_winit),
            egui_renderer: Some(egui_renderer),
            surface: Some(surface),
            surface_format: Some(surface_format),
            player: None,
            movie_size,
            movie_target: None,
            movie_texture_id: None,
            movie_viewport_scale_factor: None,
            root_error,
            font_database,
            time: Instant::now(),
            last_pointer: PhysicalPosition::new(0.0, 0.0),
            modifiers: Modifiers::default(),
            runtime: Some(runtime),
        };
        if matches!(app.screen, Screen::Playing) {
            app.start_game();
        }
        app.window.as_ref().expect("window exists").request_redraw();
        Ok(app)
    }

    /// Enters the tokio runtime context for the duration of the caller. All
    /// winit handlers that can touch the player or its futures do this, so
    /// ruffle's `tokio::spawn` calls find a reactor.
    fn enter_runtime(&self) -> Option<tokio::runtime::EnterGuard<'_>> {
        self.runtime.as_ref().map(|runtime| runtime.enter())
    }

    fn shutdown(&mut self) {
        shutdown_runtime_bound(&mut self.player, &mut self.runtime);
        drop(self.egui_renderer.take());
        drop(self.movie_target.take());
        drop(self.egui_winit.take());
        drop(self.surface.take());
        drop(self.descriptors.take());
        // The surface was created from an unsafe raw window handle, so the
        // native window must remain alive until after the surface is dropped.
        drop(self.window.take());
    }

    fn start_game(&mut self) {
        if self.player.is_some() {
            return;
        }
        let window = self.window.clone().expect("window exists");
        let descriptors = self.descriptors.clone().expect("descriptors exist");
        let (player, target) = build_player(
            &window,
            &descriptors,
            &self.event_loop,
            self.font_database.clone(),
            self.movie_size.clone(),
            self.root_error.clone(),
        )
        .expect("player construction failed");
        self.movie_target = Some(target);
        self.player = Some(player);
        self.screen = Screen::Playing;
    }

    /// Retry after a failed root-movie download: clear the error, re-fetch the
    /// movie on the existing player (or build the player if it never started).
    fn retry_play(&mut self) {
        self.root_error.lock().unwrap().take();
        self.screen = Screen::Playing;
        if self.player.is_some() {
            let player = self.player.clone().expect("player exists");
            refetch_root_movie(&player, &self.movie_size);
        } else {
            self.start_game();
        }
    }

    fn movie_rect(&self) -> egui::Rect {
        let size = (*self.movie_size.lock().unwrap()).unwrap_or((800, 600));
        // egui positions and sizes are logical points (egui-winit divides by
        // the scale factor), so the letterbox rect must come from the logical
        // window size or the movie is drawn scale_factor× too large.
        let window = self.window.as_ref().expect("window exists");
        let logical = window.inner_size().to_logical::<f64>(window.scale_factor());
        movie_rect_for(size, (logical.width, logical.height))
    }

    fn window_to_movie_viewport(&self, pos: PhysicalPosition<f64>) -> (f64, f64) {
        // Ruffle expects input in physical viewport pixels. The movie rect is
        // in logical points, so only its offset needs converting here.
        let window = self.window.as_ref().expect("window exists");
        let rect = self.movie_rect();
        window_to_movie_viewport_for(pos, &rect, window.scale_factor())
    }

    /// Keeps the renderer's viewport + the presented egui texture in sync with
    /// the movie's physical on-screen size. Called once per frame before the
    /// egui pass so window resizes also resize Ruffle's render target instead
    /// of stretching a texture that remains at the SWF's native resolution.
    fn update_movie_viewport(&mut self) {
        let movie_size = (*self.movie_size.lock().unwrap()).unwrap_or((800, 600));
        let window = self.window.as_ref().expect("window exists");
        let window_size = window.inner_size();
        let viewport_size = movie_viewport_size_for(
            movie_size,
            (window_size.width, window_size.height),
        );
        let scale_factor = window.scale_factor();
        let Some(player) = &self.player else {
            return;
        };
        if !viewport_needs_update(
            self.movie_texture_id,
            self.movie_target
                .as_ref()
                .map(|target| (target.size.width, target.size.height)),
            self.movie_viewport_scale_factor,
            viewport_size,
            scale_factor,
        ) {
            return;
        }
        let mut player_lock = player.lock().unwrap();
        player_lock.set_viewport_dimensions(ViewportDimensions {
            width: viewport_size.0,
            height: viewport_size.1,
            scale_factor,
        });
        let live = <dyn std::any::Any>::downcast_ref::<WgpuRenderBackend<TextureTarget>>(
            player_lock.renderer(),
        )
        .expect("renderer must be the wgpu backend")
        .target();
        let device = &self.descriptors.as_ref().expect("descriptors exist").device;
        let old_id = self.movie_texture_id.take();
        let egui_renderer = self.egui_renderer.as_mut().expect("egui renderer exists");
        self.movie_texture_id = Some(register_movie_texture(egui_renderer, device, live, old_id));
        self.movie_target = Some(TextureTarget {
            size: live.size,
            texture: live.get_texture(),
            format: live.format,
            buffer: None,
        });
        self.movie_viewport_scale_factor = Some(scale_factor);
    }

    fn render(&mut self, event_loop: &ActiveEventLoop) {
        let _runtime_guard = self.enter_runtime();
        self.update_movie_viewport();

        let surface_texture = {
            let surface = self.surface.as_ref().expect("surface exists");
            match surface.get_current_texture() {
                Ok(texture) => texture,
                Err(error) => match error {
                    wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                        tracing::warn!("Surface became unavailable: {error:?}, reconfiguring");
                        self.reconfigure_surface();
                        // No frame was presented, so nothing else will wake the
                        // loop; redraw once the surface is usable again.
                        self.window.as_ref().expect("window exists").request_redraw();
                        return;
                    }
                    wgpu::SurfaceError::Timeout => {
                        tracing::warn!("Surface became unavailable: {error:?}, skipping frame");
                        return;
                    }
                    wgpu::SurfaceError::OutOfMemory | wgpu::SurfaceError::Other => {
                        panic!("wgpu: surface error: {error:?}");
                    }
                },
            }
        };

        let window = self.window.as_ref().expect("window exists");
        let raw_input = self
            .egui_winit
            .as_mut()
            .expect("egui state exists")
            .take_egui_input(window);
        let size_in_pixels = [window.inner_size().width, window.inner_size().height];
        let pixels_per_point = window.scale_factor() as f32;

        // If the root movie failed to download, show the error screen.
        if let Some(message) = self.root_error.lock().unwrap().take() {
            self.screen = Screen::Error { message };
        }

        // `Context` is cheaply cloneable (Arc-backed); cloning it lets the
        // `run` closure below borrow `self` mutably without fighting the
        // borrow of `self.egui_winit`.
        let egui_ctx = self
            .egui_winit
            .as_ref()
            .expect("egui state exists")
            .egui_ctx()
            .clone();
        let full_output = egui_ctx.run(raw_input, |ctx| {
            match &self.screen {
                Screen::Disclaimer => {
                    if disclaimer_ui(ctx) {
                        self.state.disclaimer_accepted = true;
                        let _ = self.state.save(&config::config_dir());
                        self.screen = initial_screen(&self.state, &self.detected_sources);
                        if matches!(self.screen, Screen::Playing) {
                            self.start_game();
                        }
                    }
                }
                Screen::Setup { sources } => {
                    if let Some(choice) = setup_ui(ctx, sources) {
                        self.apply_migration_choice(choice);
                    }
                }
                Screen::Error { message } => {
                    match error_ui(ctx, message, self.player.is_some()) {
                        Some(ErrorAction::Retry) => self.retry_play(),
                        Some(ErrorAction::Quit) => event_loop.exit(),
                        None => {}
                    }
                }
                Screen::Playing => {
                    if let Some(player) = &self.player {
                        let mut player_lock = player.lock().unwrap();
                        player_lock.render();
                    }
                    if let Some(texture_id) = self.movie_texture_id {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::NONE)
                            .show(ctx, |_ui| {
                                let size = (*self.movie_size.lock().unwrap()).unwrap_or((800, 600));
                                let rect = self.movie_rect();
                                egui::Area::new(egui::Id::new("movie"))
                                    .fixed_pos(rect.min)
                                    .interactable(false)
                                    .show(ctx, |ui| {
                                        ui.add(
                                            egui::Image::new(egui::load::SizedTexture::new(
                                                texture_id,
                                                egui::vec2(size.0 as f32, size.1 as f32),
                                            ))
                                            .fit_to_exact_size(rect.size()),
                                        );
                                    });
                            });
                    } else {
                        loading_ui(ctx);
                    }
                }
            }
        });

        self.egui_winit
            .as_mut()
            .expect("egui state exists")
            .handle_platform_output(
                self.window.as_ref().expect("window exists"),
                full_output.platform_output,
            );

        let clipped = egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels,
            pixels_per_point,
        };
        let mut encoder = self
            .descriptors
            .as_ref()
            .expect("descriptors exist")
            .device
            .create_command_encoder(&Default::default());
        for (id, image_delta) in &full_output.textures_delta.set {
            self.egui_renderer
                .as_mut()
                .expect("egui renderer exists")
                .update_texture(
                    &self.descriptors.as_ref().expect("descriptors exist").device,
                    &self.descriptors.as_ref().expect("descriptors exist").queue,
                    *id,
                    image_delta,
                );
        }
        let mut command_buffers = self
            .egui_renderer
            .as_mut()
            .expect("egui renderer exists")
            .update_buffers(
                &self.descriptors.as_ref().expect("descriptors exist").device,
                &self.descriptors.as_ref().expect("descriptors exist").queue,
                &mut encoder,
                &clipped,
                &screen_descriptor,
            );
        let surface_view = surface_texture.texture.create_view(&Default::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("main pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            self.egui_renderer
                .as_ref()
                .expect("egui renderer exists")
                .render(&mut pass, &clipped, &screen_descriptor);
        }
        for id in &full_output.textures_delta.free {
            self.egui_renderer
                .as_mut()
                .expect("egui renderer exists")
                .free_texture(id);
        }
        command_buffers.push(encoder.finish());
        self.descriptors
            .as_ref()
            .expect("descriptors exist")
            .queue
            .submit(command_buffers);
        surface_texture.present();
    }

    fn reconfigure_surface(&mut self) {
        let window = self.window.as_ref().expect("window exists");
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let surface = self.surface.as_ref().expect("surface exists");
        let device = &self.descriptors.as_ref().expect("descriptors exist").device;
        surface.configure(
            device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.surface_format.expect("surface format"),
                width: size.width,
                height: size.height,
                present_mode: wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: Vec::new(),
            },
        );
    }

    fn apply_migration_choice(&mut self, choice: Option<usize>) {
        let detected: Vec<MigratedSource> = match &self.screen {
            Screen::Setup { sources } => sources.clone(),
            _ => Vec::new(),
        };
        let source = choice.map(|index| detected[index].id.clone());
        let result = if let Some(index) = choice {
            migration::sources()
                .iter()
                .find(|s| s.id == detected[index].id.as_str())
                .and_then(|s| {
                    let dirs: Vec<_> = s
                        .roots
                        .iter()
                        .flat_map(|root| migration::detect(root, s.include_proxy_host))
                        .collect();
                    (!dirs.is_empty()).then(|| migration::copy_source(&dirs, &config::save_dir()))
                })
        } else {
            None
        };
        let persisted = should_persist_migration(&result);
        match result {
            Some(Ok(copied)) => {
                tracing::info!("Copied {copied} save files from {source:?}");
            }
            Some(Err(error)) => {
                tracing::error!(
                    "Migration copy failed, setup screen will show again on next run: {error}"
                );
            }
            None => {}
        }
        if persisted {
            self.persist_migration_choice(source);
        }
        self.screen = Screen::Playing;
        self.start_game();
    }

    /// Records the user's migration choice; only called after a successful
    /// copy or an explicit decline, never after a failed copy.
    fn persist_migration_choice(&mut self, source: Option<String>) {
        self.state.migration = Some(crate::config::MigrationChoice {
            source,
            copied_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
        let _ = self.state.save(&config::config_dir());
    }
}

/// True when the presented movie viewport must be re-synced: either no egui
/// texture has been registered yet (the SWF header may report a stage size
/// equal to the placeholder, so a size mismatch alone would miss it) or the
/// physical size/scale factor differs from the currently presented one.
fn viewport_needs_update(
    movie_texture_id: Option<egui::TextureId>,
    movie_target_size: Option<(u32, u32)>,
    current_scale_factor: Option<f64>,
    size: (u32, u32),
    scale_factor: f64,
) -> bool {
    movie_texture_id.is_none()
        || movie_target_size.is_none_or(|target| target != size)
        || current_scale_factor != Some(scale_factor)
}

/// (Re)registers the renderer's target texture with egui, freeing the previous
/// registration. The view is recreated every call because the target texture is
/// replaced whenever the renderer's viewport is resized.
fn register_movie_texture(
    egui_renderer: &mut egui_wgpu::Renderer,
    device: &wgpu::Device,
    target: &TextureTarget,
    old_id: Option<egui::TextureId>,
) -> egui::TextureId {
    let view = target.texture.create_view(&Default::default());
    if let Some(old_id) = old_id {
        egui_renderer.free_texture(&old_id);
    }
    egui_renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear)
}

/// Whether a migration copy result should record the user's choice: only a
/// successful copy (or an explicit decline) persists, so a failed copy leaves
/// the setup screen visible on the next run.
fn should_persist_migration(result: &Option<std::io::Result<usize>>) -> bool {
    !matches!(result, Some(Err(_)))
}

/// Aspect-fits `movie` inside `window` (a logical-point size), centered. Both
/// sides clamp to a minimum of 1 unit so the result is always a valid rect.
fn movie_rect_for(movie: (u32, u32), window: (f64, f64)) -> egui::Rect {
    let (mw, mh) = (movie.0.max(1) as f32, movie.1.max(1) as f32);
    let (ww, wh) = (window.0.max(1.0) as f32, window.1.max(1.0) as f32);
    let scale = (ww / mw).min(wh / mh);
    let (w, h) = (mw * scale, mh * scale);
    egui::Rect::from_min_size(egui::pos2((ww - w) / 2.0, (wh - h) / 2.0), egui::vec2(w, h))
}

/// Returns the physical-pixel render resolution for an aspect-fitted movie.
fn movie_viewport_size_for(movie: (u32, u32), window: (u32, u32)) -> (u32, u32) {
    let rect = movie_rect_for(movie, (window.0 as f64, window.1 as f64));
    (
        (rect.width().round() as u32).max(1),
        (rect.height().round() as u32).max(1),
    )
}

/// Maps a physical pointer position into Ruffle's physical-pixel viewport.
fn window_to_movie_viewport_for(
    pos: PhysicalPosition<f64>,
    rect: &egui::Rect,
    scale_factor: f64,
) -> (f64, f64) {
    (
        pos.x - f64::from(rect.min.x) * scale_factor,
        pos.y - f64::from(rect.min.y) * scale_factor,
    )
}

impl ApplicationHandler<RuffleEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: RuffleEvent) {
        let _runtime_guard = self.enter_runtime();
        match event {
            RuffleEvent::TaskPoll(runnable) => {
                runnable.run();
            }
        }
        self.window.as_ref().expect("window exists").request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let _runtime_guard = self.enter_runtime();
        if let WindowEvent::RedrawRequested = &event {
            self.render(event_loop);
            return;
        }

        // Feed the event to egui first (it needs everything to drive its widgets).
        {
            let window = self.window.as_ref().expect("window exists");
            let egui_winit = self.egui_winit.as_mut().expect("egui state exists");
            let _ = egui_winit.on_window_event(window, &event);
        }

        match &self.screen {
            Screen::Playing => {
                if let Some(player) = &self.player {
                    let mut player_lock = player.lock().unwrap();
                    match &event {
                        WindowEvent::CursorMoved { position, .. } => {
                            if *position == self.last_pointer {
                                return;
                            }
                            self.last_pointer = *position;
                            let (x, y) = self.window_to_movie_viewport(*position);
                            player_lock.handle_event(PlayerEvent::MouseMove {
                                x,
                                y,
                                source: MouseInputSource::Mouse,
                            });
                        }
                        WindowEvent::MouseInput { button, state, .. } => {
                            let (x, y) = self.window_to_movie_viewport(self.last_pointer);
                            let button = match button {
                                winit::event::MouseButton::Left => MouseButton::Left,
                                winit::event::MouseButton::Right => MouseButton::Right,
                                winit::event::MouseButton::Middle => MouseButton::Middle,
                                _ => MouseButton::Unknown,
                            };
                            let event = match state {
                                ElementState::Pressed => PlayerEvent::MouseDown {
                                    x,
                                    y,
                                    button,
                                    index: None,
                                    source: MouseInputSource::Mouse,
                                },
                                ElementState::Released => PlayerEvent::MouseUp {
                                    x,
                                    y,
                                    button,
                                    source: MouseInputSource::Mouse,
                                },
                            };
                            player_lock.handle_event(event);
                        }
                        WindowEvent::MouseWheel { delta, .. } => {
                            let delta = match delta {
                                MouseScrollDelta::LineDelta(_, dy) => {
                                    MouseWheelDelta::Lines((*dy).into())
                                }
                                MouseScrollDelta::PixelDelta(pos) => {
                                    MouseWheelDelta::Pixels(pos.y)
                                }
                            };
                            player_lock.handle_event(PlayerEvent::MouseWheel { delta });
                        }
                        WindowEvent::CursorEntered { .. } => {
                            player_lock.set_mouse_in_stage(true);
                        }
                        WindowEvent::CursorLeft { .. } => {
                            player_lock.set_mouse_in_stage(false);
                            player_lock.handle_event(PlayerEvent::MouseLeave);
                        }
                        WindowEvent::Focused(focused) => {
                            player_lock.handle_event(if *focused {
                                PlayerEvent::FocusGained
                            } else {
                                PlayerEvent::FocusLost
                            });
                        }
                        WindowEvent::KeyboardInput { event, .. } => {
                            let key = winit_input_to_ruffle_key_descriptor(event);
                            match event.state {
                                ElementState::Pressed => {
                                    player_lock.handle_event(PlayerEvent::KeyDown { key });
                                    if let Some(control_code) =
                                        winit_to_ruffle_text_control(event, self.modifiers)
                                    {
                                        player_lock
                                            .handle_event(PlayerEvent::TextControl {
                                                code: control_code,
                                            });
                                    } else if let Some(text) = &event.text {
                                        for codepoint in text.chars() {
                                            player_lock
                                                .handle_event(PlayerEvent::TextInput { codepoint });
                                        }
                                    }
                                }
                                ElementState::Released => {
                                    player_lock.handle_event(PlayerEvent::KeyUp { key });
                                }
                            }
                        }
                        WindowEvent::Ime(ime) => match ime {
                            Ime::Enabled => {}
                            Ime::Preedit(text, cursor) => {
                                player_lock.handle_event(PlayerEvent::Ime(ImeEvent::Preedit(
                                    text.clone(),
                                    *cursor,
                                )));
                            }
                            Ime::Commit(text) => {
                                player_lock.handle_event(PlayerEvent::Ime(ImeEvent::Commit(
                                    text.clone(),
                                )));
                            }
                            Ime::Disabled => {}
                        },
                        _ => {}
                    }
                }
            }
            _ => {
                // Overlay screens: egui widgets handle their own clicks; the
                // actions are applied below in the egui pass.
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.reconfigure_surface();
            }
            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.modifiers = new_modifiers;
            }
            _ => {}
        }
        self.window.as_ref().expect("window exists").request_redraw();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _runtime_guard = self.enter_runtime();
        if matches!(self.screen, Screen::Playing) && self.player.is_some() {
            let new_time = Instant::now();
            let dt = FloatDuration::from_std(new_time.duration_since(self.time));
            if dt.as_millis() > 0.0 {
                self.time = new_time;
                let next_frame = self.player.as_ref().map(|player| {
                    let mut player_lock = player.lock().unwrap();
                    player_lock.tick(dt);
                    new_time + player_lock.time_til_next_frame()
                });
                if let Some(next_frame) = next_frame {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(next_frame));
                }
            }
            if self
                .player
                .as_ref()
                .is_some_and(|player| player.lock().unwrap().needs_render())
            {
                self.window.as_ref().expect("window exists").request_redraw();
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown();
    }
}

// Pure egui widgets for the overlay screens. (These live in app.rs for now;
// they are the designated growth point for future settings UI.)
fn disclaimer_ui(ctx: &egui::Context) -> bool {
    let mut continue_clicked = false;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x66, 0x00, 0x00)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.heading("Disclaimer");
                ui.add_space(24.0);
                ui.label(
                    "This is a 3rd party launcher that is not supported nor endorsed by Artix Entertainment.",
                );
                ui.label("By clicking 'Continue', you agree to use this launcher at your own risk.");
                ui.add_space(32.0);
                if ui.button("Continue").clicked() {
                    continue_clicked = true;
                }
            });
        });
    continue_clicked
}

fn setup_ui(ctx: &egui::Context, sources: &[MigratedSource]) -> Option<Option<usize>> {
    let mut choice = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x00, 0x33, 0x00)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.heading("Save data found");
                ui.add_space(16.0);
                ui.label("We found save data from another DragonFable launcher.");
                ui.label("Would you like to copy it into DragonFable?");
                ui.add_space(24.0);
            });
            for (index, source) in sources.iter().enumerate() {
                if ui.button(format!("Copy from {}", source.name)).clicked() {
                    choice = Some(Some(index));
                }
            }
            if ui.button("Don't copy").clicked() {
                choice = Some(None);
            }
        });
    choice
}

/// The action the user picked on the error screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorAction {
    Retry,
    Quit,
}

fn error_ui(ctx: &egui::Context, message: &str, can_retry: bool) -> Option<ErrorAction> {
    error_ui_with_geometry(ctx, message, can_retry).0
}

/// [`error_ui`] plus the button rects, so tests can drive clicks without
/// guessing layout metrics.
fn error_ui_with_geometry(
    ctx: &egui::Context,
    message: &str,
    can_retry: bool,
) -> (Option<ErrorAction>, Vec<(ErrorAction, egui::Rect)>) {
    let mut action = None;
    let mut buttons = Vec::new();
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x66, 0x00, 0x00)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading("Failed to load DragonFable");
                ui.add_space(16.0);
                ui.label(message);
                ui.add_space(24.0);
                if can_retry {
                    let response = ui.button("Retry");
                    if response.clicked() {
                        action = Some(ErrorAction::Retry);
                    }
                    buttons.push((ErrorAction::Retry, response.rect));
                    ui.add_space(8.0);
                }
                let response = ui.button("Quit");
                if response.clicked() {
                    action = Some(ErrorAction::Quit);
                }
                buttons.push((ErrorAction::Quit, response.rect));
            });
        });
    (action, buttons)
}

fn loading_ui(ctx: &egui::Context) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(200.0);
                ui.label("Loading DragonFable…");
            });
        });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct RuntimeDropProbe(Arc<AtomicBool>);

    impl Drop for RuntimeDropProbe {
        fn drop(&mut self) {
            self.0.store(
                tokio::runtime::Handle::try_current().is_ok(),
                Ordering::Relaxed,
            );
        }
    }

    #[test]
    fn runtime_bound_resource_drops_inside_runtime_context() {
        let dropped_inside_runtime = Arc::new(AtomicBool::new(false));
        let mut resource = Some(RuntimeDropProbe(dropped_inside_runtime.clone()));
        let mut runtime = Some(tokio::runtime::Runtime::new().unwrap());

        shutdown_runtime_bound(&mut resource, &mut runtime);
        shutdown_runtime_bound(&mut resource, &mut runtime);

        assert!(dropped_inside_runtime.load(Ordering::Relaxed));
        assert!(resource.is_none());
        assert!(runtime.is_none());
    }

    fn error_screen() -> (egui::Context, Option<ErrorAction>, Vec<(ErrorAction, egui::Rect)>) {
        let ctx = egui::Context::default();
        let mut result = (None, Vec::new());
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| result = error_ui_with_geometry(ctx, "boom", true),
        );
        (ctx, result.0, result.1)
    }

    fn error_ui_clicked_at(pos: egui::Pos2, can_retry: bool) -> Option<ErrorAction> {
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        // First pass lays out the widgets; the click in the second pass is
        // hit-tested against the rects registered by this pass.
        let _ = ctx.run(input(), |ctx| {
            error_ui_with_geometry(ctx, "boom", can_retry);
        });
        let mut action = None;
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                ..input()
            },
            |ctx| action = error_ui_with_geometry(ctx, "boom", can_retry).0,
        );
        action
    }

    #[test]
    fn error_ui_without_a_click_returns_no_action() {
        let (_, action, _) = error_screen();
        assert_eq!(action, None);
    }

    #[test]
    fn error_ui_retry_click_returns_retry() {
        let (_, _, buttons) = error_screen();
        let rect = buttons
            .iter()
            .find(|(action, _)| matches!(action, ErrorAction::Retry))
            .expect("Retry button")
            .1;
        assert_eq!(error_ui_clicked_at(rect.center(), true), Some(ErrorAction::Retry));
    }

    #[test]
    fn error_ui_quit_click_returns_quit() {
        let (_, _, buttons) = error_screen();
        let rect = buttons
            .iter()
            .find(|(action, _)| matches!(action, ErrorAction::Quit))
            .expect("Quit button")
            .1;
        assert_eq!(error_ui_clicked_at(rect.center(), true), Some(ErrorAction::Quit));
    }

    #[test]
    fn error_ui_without_retry_offers_only_quit() {
        let ctx = egui::Context::default();
        let mut result = (None, Vec::new());
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| result = error_ui_with_geometry(ctx, "boom", false),
        );
        assert_eq!(result.0, None);
        assert_eq!(result.1.len(), 1);
        assert!(matches!(result.1[0].0, ErrorAction::Quit));
    }

    #[test]
    fn error_ui_ignores_clicks_off_the_buttons() {
        assert_eq!(error_ui_clicked_at(egui::Pos2::new(5.0, 5.0), true), None);
    }

    #[test]
    fn viewport_needs_update_when_no_texture_registered_even_if_sizes_match() {
        assert!(viewport_needs_update(
            None,
            Some((800, 600)),
            Some(1.0),
            (800, 600),
            1.0
        ));
        assert!(viewport_needs_update(
            None,
            None,
            None,
            (800, 600),
            1.0
        ));
    }

    #[test]
    fn viewport_needs_update_when_the_movie_size_changed() {
        assert!(viewport_needs_update(
            Some(egui::TextureId::User(1)),
            Some((800, 600)),
            Some(1.0),
            (750, 550),
            1.0
        ));
    }

    #[test]
    fn viewport_needs_update_when_the_scale_factor_changed() {
        assert!(viewport_needs_update(
            Some(egui::TextureId::User(1)),
            Some((800, 600)),
            Some(1.0),
            (800, 600),
            1.5
        ));
    }

    #[test]
    fn viewport_does_not_need_update_when_texture_registered_and_sizes_match() {
        assert!(!viewport_needs_update(
            Some(egui::TextureId::User(1)),
            Some((800, 600)),
            Some(1.5),
            (800, 600),
            1.5
        ));
    }

    #[test]
    fn movie_rect_fills_window_when_aspects_match() {
        let rect = movie_rect_for((800, 600), (800.0, 600.0));
        assert_eq!(rect.min, egui::pos2(0.0, 0.0));
        assert_eq!(rect.max, egui::pos2(800.0, 600.0));
    }

    #[test]
    fn movie_rect_letterboxes_a_wider_window() {
        let rect = movie_rect_for((800, 600), (1600.0, 600.0));
        assert_eq!(rect.min.x, 400.0);
        assert_eq!(rect.min.y, 0.0);
        assert_eq!(rect.width(), 800.0);
        assert_eq!(rect.height(), 600.0);
    }

    #[test]
    fn movie_rect_letterboxes_a_shorter_window() {
        let rect = movie_rect_for((800, 600), (800.0, 300.0));
        assert_eq!(rect.min.x, 200.0);
        assert_eq!(rect.min.y, 0.0);
        assert_eq!(rect.width(), 400.0);
        assert_eq!(rect.height(), 300.0);
    }

    #[test]
    fn movie_rect_clamps_zero_sizes() {
        let rect = movie_rect_for((0, 0), (0.0, 0.0));
        assert!(rect.width() > 0.0);
        assert!(rect.height() > 0.0);
    }

    #[test]
    fn movie_rect_uses_logical_points_at_hidpi_scale_factors() {
        // 1920x1080 physical @ scale 1.5 -> 1280x720 logical points. The rect
        // is computed from the logical size: the 4:3 movie letterboxes to
        // 960x720 at (160, 0), whereas physical-size math would produce a
        // 1440x1080 rect that is drawn 1.5× too large and cropped.
        let rect = movie_rect_for((800, 600), (1280.0, 720.0));
        assert!((rect.min.x - 160.0).abs() < 1e-3);
        assert!(rect.min.y.abs() < 1e-3);
        assert!((rect.width() - 960.0).abs() < 1e-3);
        assert!((rect.height() - 720.0).abs() < 1e-3);
    }

    #[test]
    fn movie_viewport_resolution_tracks_the_aspect_fitted_window_size() {
        assert_eq!(movie_viewport_size_for((800, 600), (1600, 1200)), (1600, 1200));
        assert_eq!(movie_viewport_size_for((800, 600), (1600, 600)), (800, 600));
        assert_eq!(movie_viewport_size_for((800, 600), (800, 300)), (400, 300));
        assert_eq!(movie_viewport_size_for((0, 0), (0, 0)), (1, 1));
    }

    #[test]
    fn window_to_movie_viewport_maps_into_the_letterboxed_rect() {
        let rect = movie_rect_for((800, 600), (1600.0, 600.0));
        // Center of the window == center of the 800x600 physical viewport.
        let (x, y) =
            window_to_movie_viewport_for(PhysicalPosition::new(800.0, 300.0), &rect, 1.0);
        assert!((x - 400.0).abs() < 1e-3);
        assert!((y - 300.0).abs() < 1e-3);
        // Left edge of the movie rect == viewport x 0.
        let (x, _) =
            window_to_movie_viewport_for(PhysicalPosition::new(400.0, 0.0), &rect, 1.0);
        assert!(x.abs() < 1e-3);
        // Outside the movie rect maps to negative coordinates.
        let (x, _) =
            window_to_movie_viewport_for(PhysicalPosition::new(0.0, 0.0), &rect, 1.0);
        assert!(x < 0.0);
    }

    #[test]
    fn window_to_movie_viewport_preserves_physical_pixels_on_hidpi() {
        // 1920x1080 physical @ scale 1.5 -> 1280x720 logical points; the
        // movie rect is (160, 0)..(1120, 720), or 1440x1080 physical pixels.
        let rect = movie_rect_for((800, 600), (1280.0, 720.0));
        // Physical center maps to the center of that physical viewport.
        let (x, y) =
            window_to_movie_viewport_for(PhysicalPosition::new(960.0, 540.0), &rect, 1.5);
        assert!((x - 720.0).abs() < 1e-3);
        assert!((y - 540.0).abs() < 1e-3);
        // Physical (240, 0) is the movie rect's left edge.
        let (x, _) =
            window_to_movie_viewport_for(PhysicalPosition::new(240.0, 0.0), &rect, 1.5);
        assert!(x.abs() < 1e-3);
    }

    #[test]
    fn migration_choice_persists_only_on_success_or_decline() {
        assert!(should_persist_migration(&None));
        assert!(should_persist_migration(&Some(Ok(3))));
        assert!(!should_persist_migration(&Some(Err(std::io::Error::other(
            "copy failed"
        )))));
    }
}
