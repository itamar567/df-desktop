//! The winit application: window, wgpu + egui setup, screen rendering, and
//! input forwarding to the player.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    /// The GPU texture the player renders into, at the movie's native size.
    /// The renderer owns the authoritative target; this mirrors it so the app
    /// can detect when the movie size changes and re-register the egui texture.
    movie_target: Option<TextureTarget>,
    movie_texture_id: Option<egui::TextureId>,
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
        let instance = create_wgpu_instance(wgpu::Backends::all(), wgpu::BackendOptions::default());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(window.as_ref())?)?
        };
        let (adapter, device, queue) =
            futures::executor::block_on(request_adapter_and_device(
                wgpu::Backends::all(),
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
        let window = self.window.as_ref().expect("window exists").inner_size();
        movie_rect_for(size, (window.width, window.height))
    }

    fn window_to_movie(&self, pos: PhysicalPosition<f64>) -> (f64, f64) {
        let rect = self.movie_rect();
        let size = (*self.movie_size.lock().unwrap()).unwrap_or((800, 600));
        window_to_movie_for(pos, &rect, size)
    }

    /// Keeps the renderer's viewport + the presented egui texture in sync with
    /// the movie's size (the placeholder `(800, 600)` until the SWF header
    /// arrives). Called once per frame before the egui pass.
    fn update_movie_viewport(&mut self) {
        let size = (*self.movie_size.lock().unwrap()).unwrap_or((800, 600));
        let Some(player) = &self.player else {
            return;
        };
        if self.movie_target.as_ref().is_some_and(|target| {
            target.size.width == size.0 && target.size.height == size.1
        }) {
            return;
        }
        let mut player_lock = player.lock().unwrap();
        player_lock.set_viewport_dimensions(ViewportDimensions {
            width: size.0,
            height: size.1,
            scale_factor: 1.0,
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
    }

    fn render(&mut self) {
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
                    if error_ui(ctx, message, self.player.is_some()) {
                        self.retry_play();
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
                                ctx.request_repaint();
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
        match result {
            Some(Ok(copied)) => tracing::info!("Copied {copied} save files from {source:?}"),
            Some(Err(error)) => tracing::warn!("Migration copy failed: {error}"),
            None => {}
        }
        self.state.migration = Some(crate::config::MigrationChoice {
            source,
            copied_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
        let _ = self.state.save(&config::config_dir());
        self.screen = Screen::Playing;
        self.start_game();
    }
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

/// Aspect-fits `movie` inside `window`, centered. Both sides clamp to 1px so
/// the result is always a valid rect.
fn movie_rect_for(movie: (u32, u32), window: (u32, u32)) -> egui::Rect {
    let (mw, mh) = (movie.0.max(1) as f32, movie.1.max(1) as f32);
    let (ww, wh) = (window.0.max(1) as f32, window.1.max(1) as f32);
    let scale = (ww / mw).min(wh / mh);
    let (w, h) = (mw * scale, mh * scale);
    egui::Rect::from_min_size(egui::pos2((ww - w) / 2.0, (wh - h) / 2.0), egui::vec2(w, h))
}

fn window_to_movie_for(
    pos: PhysicalPosition<f64>,
    rect: &egui::Rect,
    movie: (u32, u32),
) -> (f64, f64) {
    let x = (pos.x as f32 - rect.min.x) * movie.0.max(1) as f32 / rect.width();
    let y = (pos.y as f32 - rect.min.y) * movie.1.max(1) as f32 / rect.height();
    (x as f64, y as f64)
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
            self.render();
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
                            let (x, y) = self.window_to_movie(*position);
                            player_lock.handle_event(PlayerEvent::MouseMove {
                                x,
                                y,
                                source: MouseInputSource::Mouse,
                            });
                        }
                        WindowEvent::MouseInput { button, state, .. } => {
                            let (x, y) = self.window_to_movie(self.last_pointer);
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
                        WindowEvent::CursorLeft { .. } => {
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

fn error_ui(ctx: &egui::Context, message: &str, can_retry: bool) -> bool {
    let mut retry = false;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x66, 0x00, 0x00)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading("Failed to load DragonFable");
                ui.add_space(16.0);
                ui.label(message);
                ui.add_space(24.0);
                if can_retry && ui.button("Retry").clicked() {
                    retry = true;
                }
            });
        });
    retry
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
    use super::*;

    #[test]
    fn movie_rect_fills_window_when_aspects_match() {
        let rect = movie_rect_for((800, 600), (800, 600));
        assert_eq!(rect.min, egui::pos2(0.0, 0.0));
        assert_eq!(rect.max, egui::pos2(800.0, 600.0));
    }

    #[test]
    fn movie_rect_letterboxes_a_wider_window() {
        let rect = movie_rect_for((800, 600), (1600, 600));
        assert_eq!(rect.min.x, 400.0);
        assert_eq!(rect.min.y, 0.0);
        assert_eq!(rect.width(), 800.0);
        assert_eq!(rect.height(), 600.0);
    }

    #[test]
    fn movie_rect_letterboxes_a_shorter_window() {
        let rect = movie_rect_for((800, 600), (800, 300));
        assert_eq!(rect.min.x, 200.0);
        assert_eq!(rect.min.y, 0.0);
        assert_eq!(rect.width(), 400.0);
        assert_eq!(rect.height(), 300.0);
    }

    #[test]
    fn movie_rect_clamps_zero_sizes() {
        let rect = movie_rect_for((0, 0), (0, 0));
        assert!(rect.width() > 0.0);
        assert!(rect.height() > 0.0);
    }

    #[test]
    fn window_to_movie_maps_into_the_letterboxed_rect() {
        let rect = movie_rect_for((800, 600), (1600, 600));
        // Center of the window == center of the movie.
        let (x, y) = window_to_movie_for(PhysicalPosition::new(800.0, 300.0), &rect, (800, 600));
        assert!((x - 400.0).abs() < 1e-3);
        assert!((y - 300.0).abs() < 1e-3);
        // Left edge of the movie rect == movie x 0.
        let (x, _) = window_to_movie_for(PhysicalPosition::new(400.0, 0.0), &rect, (800, 600));
        assert!(x.abs() < 1e-3);
        // Outside the movie rect maps to negative coordinates.
        let (x, _) = window_to_movie_for(PhysicalPosition::new(0.0, 0.0), &rect, (800, 600));
        assert!(x < 0.0);
    }
}
