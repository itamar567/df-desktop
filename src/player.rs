//! Player construction and the winit-backed future executor.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context};
use ruffle_core::backend::navigator::{OwnedFuture, SocketMode};
use ruffle_core::config::Letterbox;
use ruffle_core::font::DefaultFont;
use ruffle_core::{Player, PlayerBuilder};
use ruffle_frontend_utils::backends::audio::CpalAudioBackend;
use ruffle_frontend_utils::backends::navigator::{
    ExternalNavigatorBackend, FutureSpawner,
};
use ruffle_frontend_utils::backends::storage::DiskStorageBackend;
use ruffle_frontend_utils::content::{ContentDescriptor, PlayingContent};
use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_video_software::backend::SoftwareVideoBackend;
use url::Url;
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::config::{self, BASE_DOMAIN, CACHE_MAX_BYTES, GAME_URL};
use crate::log::{LogListener, SharedLogState};
use crate::navigator::DesktopNavigatorInterface;
use crate::ui::MinimalUiBackend;
use dragonfable_cache::{CacheHandle, Config, DragonFableCachingNavigator};

/// Events the winit event loop processes for us.
pub enum RuffleEvent {
    TaskPoll(async_task::Runnable<()>),
    LogChanged,
    LogProcess(crate::log_process::LogProcessEvent),
}

/// A bare-bones executor that schedules futures on the winit event loop,
/// mirroring ruffle_desktop's `WinitExecutor`.
#[derive(Clone)]
struct WinitExecutor {
    event_loop: EventLoopProxy<RuffleEvent>,
}

impl<E: std::error::Error + 'static> FutureSpawner<E> for WinitExecutor {
    fn spawn(&self, future: OwnedFuture<(), E>) {
        let future = async {
            if let Err(error) = future.await {
                tracing::error!("Async error: {error}");
            }
        };
        let event_loop = self.event_loop.clone();
        let scheduler = move |task| {
            if event_loop.send_event(RuffleEvent::TaskPoll(task)).is_err() {
                tracing::error!("Couldn't schedule task - event loop is closed");
            }
        };
        let (runnable, task) = async_task::Builder::new().spawn_local(|_| future, scheduler);
        task.detach();
        runnable.schedule();
    }
}

/// A twip is 1/20th of a pixel; negative stage dimensions clamp to zero.
fn twips_to_pixels(twips: i32) -> u32 {
    (twips.max(0) / 20) as u32
}

/// The host to cache against, falling back to the hardcoded constant for URLs
/// without a host (e.g. local files).
fn base_domain(movie_url: &Url) -> &str {
    movie_url.host_str().unwrap_or(BASE_DOMAIN)
}

/// Builds the player: caching navigator, wgpu renderer, audio, storage and the
/// minimal UI backend, then starts the root movie download.
///
/// Returns the player plus a [`TextureTarget`] sharing the GPU texture the
/// renderer draws into, so the app can present it (e.g. register the texture
/// view with egui). The renderer owns the authoritative target and replaces it
/// whenever the viewport is resized, so after `player.set_viewport_dimensions`
/// re-fetch the live target via
/// `player.renderer().downcast_ref::<WgpuRenderBackend<TextureTarget>>()`.
#[allow(clippy::too_many_arguments)]
pub fn build_player(
    window: &Arc<Window>,
    descriptors: &Arc<Descriptors>,
    event_loop: &EventLoopProxy<RuffleEvent>,
    font_database: Rc<fontdb::Database>,
    movie_size: Arc<Mutex<Option<(u32, u32)>>>,
    root_error: Arc<Mutex<Option<String>>>,
    log_state: SharedLogState,
) -> anyhow::Result<(Arc<Mutex<Player>>, TextureTarget, CacheHandle)> {
    let future_spawner = WinitExecutor { event_loop: event_loop.clone() };

    let movie_url = Url::parse(GAME_URL).context("hardcoded game URL must parse")?;
    let base_domain = base_domain(&movie_url).to_string();

    let navigator = DragonFableCachingNavigator::new(
        ExternalNavigatorBackend::new(
            movie_url.clone(),
            None,
            None,
            future_spawner.clone(),
            None,
            true,
            Default::default(),
            SocketMode::Allow,
            Rc::new(PlayingContent::DirectFile(ContentDescriptor::new_remote(
                movie_url.clone(),
            ))),
            DesktopNavigatorInterface { log_state: log_state.clone() },
        ),
        Config {
            cache_dir: config::cache_dir(),
            base_domain,
            max_cache_bytes: CACHE_MAX_BYTES,
        },
        future_spawner,
    );
    let cache_handle = navigator.cache_handle();

    let movie_size_px = movie_size.lock().unwrap().unwrap_or((800, 600));
    let render_target = TextureTarget::new(&descriptors.device, movie_size_px)
        .map_err(|e| anyhow!("failed to create render target: {e}"))?;
    let movie_target = TextureTarget {
        size: render_target.size,
        texture: render_target.get_texture(),
        format: render_target.format,
        // Readback stays with the renderer's own copy; the app only presents.
        buffer: None,
    };
    let renderer = WgpuRenderBackend::new(descriptors.clone(), render_target)
        .map_err(|e| anyhow!("failed to create wgpu render backend: {e}"))?;

    let log_listener = Arc::new(LogListener::new(log_state.clone()));
    let listeners = Arc::new(Mutex::new(vec![log_listener as Arc<dyn ruffle_core::local_connection::LocalConnectionListener>]));
    
    let mut builder = PlayerBuilder::new()
        .with_navigator(navigator)
        .with_renderer(renderer)
        .with_storage(Box::new(DiskStorageBackend::new(config::save_dir())))
        .with_ui(MinimalUiBackend::new(
            window.clone(),
            font_database,
            root_error,
        ))
        .with_video(SoftwareVideoBackend::new())
        .with_autoplay(true)
        .with_letterbox(Letterbox::On)
        .with_max_execution_duration(Duration::MAX)
        .with_local_connection_listeners(listeners);

    if let Ok(audio) = CpalAudioBackend::new(None) {
        builder = builder.with_audio(audio);
    } else {
        tracing::warn!("No audio device available; running without audio");
    }

    let player = builder.build();

    {
        let mut player_lock = player.lock().unwrap();
        // Map the SWF's generic `_sans`/`_serif`/`_typewriter` device fonts to
        // real system fonts, mirroring ruffle_desktop (each name is tried in
        // order until one is found in the font database).
        player_lock.set_default_font(
            DefaultFont::Serif,
            vec![
                "Times New Roman".into(),
                "Tinos".into(),
                "Liberation Serif".into(),
                "DejaVu Serif".into(),
            ],
        );
        player_lock.set_default_font(
            DefaultFont::Sans,
            vec![
                "Arial".into(),
                "Arimo".into(),
                "Liberation Sans".into(),
                "DejaVu Sans".into(),
            ],
        );
        player_lock.set_default_font(
            DefaultFont::Typewriter,
            vec![
                "Courier New".into(),
                "Cousine".into(),
                "Liberation Mono".into(),
                "DejaVu Sans Mono".into(),
            ],
        );
    }

    refetch_root_movie(&player, &movie_size);

    Ok((player, movie_target, cache_handle))
}

/// Kicks off (or re-kicks off, after a failed download) the root movie fetch,
/// recording the SWF stage size into `movie_size` once the header arrives.
pub fn refetch_root_movie(
    player: &Arc<Mutex<Player>>,
    movie_size: &Arc<Mutex<Option<(u32, u32)>>>,
) {
    let mut player_lock = player.lock().unwrap();
    let movie_size = movie_size.clone();
    player_lock.fetch_root_movie(
        GAME_URL.to_string(),
        Vec::new(),
        Box::new(move |header| {
            let stage = header.stage_size();
            *movie_size.lock().unwrap() = Some((
                twips_to_pixels(stage.width().get()),
                twips_to_pixels(stage.height().get()),
            ));
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twips_convert_to_pixels_by_dividing_by_20() {
        assert_eq!(twips_to_pixels(400), 20);
        assert_eq!(twips_to_pixels(399), 19);
        assert_eq!(twips_to_pixels(0), 0);
    }

    #[test]
    fn negative_twips_clamp_to_zero_pixels() {
        assert_eq!(twips_to_pixels(-100), 0);
    }

    #[test]
    fn base_domain_comes_from_the_game_url() {
        let url = Url::parse("https://play.dragonfable.com/game/DFLoader.swf").unwrap();
        assert_eq!(base_domain(&url), "play.dragonfable.com");
    }

    #[test]
    fn hostless_urls_fall_back_to_the_hardcoded_domain() {
        let url = Url::parse("file:///tmp/movie.swf").unwrap();
        assert_eq!(base_domain(&url), BASE_DOMAIN);
    }
}
