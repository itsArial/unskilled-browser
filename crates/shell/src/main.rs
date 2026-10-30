// Release builds on Windows are GUI apps: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod clipboard;
mod clock;
mod host;
mod inspector;
mod keys;
mod lock;
mod menu;
mod panel;
mod sheet;
mod shell;
#[cfg(unix)]
mod signals;
mod theme;
mod update;
mod vpn;

use lock::{LockScreen, Submit};
use shell::Shell;
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use ui::{Renderer, Scene};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{CursorIcon, ResizeDirection, Window, WindowId},
};

/// Chromium wants its work pumped at least this often even when it schedules nothing.
const PUMP_FALLBACK: Duration = Duration::from_millis(33);
/// Pixels per wheel notch.
const LINE: f32 = 60.0;
/// Width of the invisible band along the window edges that resizes it (no window frame here).
const EDGE: f32 = 6.0;
/// Encrypted snapshot of the running session, so a crash or power loss loses little.
const CHECKPOINT: Duration = Duration::from_secs(10 * 60);

enum Outcome {
    Created(Result<vault::Session, vault::Error>),
    Unlocked(Result<vault::Session, vault::Error>),
    /// The session comes back either way; the error says whether the PIN was set.
    Upgraded(vault::Session, Result<(), vault::Error>),
}

#[derive(Debug)]
enum UserEvent {
    /// Something outside the event loop (VPN watcher, shield) changed what's on screen.
    Redraw,
    /// CEF asked for `engine::pump()` after this many milliseconds.
    Pump(i64),
    /// The vault worker finished unlocking (or creating).
    Unlocked,
    /// SIGTERM/SIGINT/SIGHUP (logout, shutdown, Ctrl+C): save the vault and exit.
    Quit,
    /// A newer build was installed over this one.
    UpdateReady,
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

impl Gpu {
    async fn new(window: Arc<Window>) -> Self {
        // Chromium's shared frames arrive as DMA-BUFs on Linux (Vulkan import) and as D3D11
        // handles on Windows (D3D12 import).
        let backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::VULKAN };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window.clone()).expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .expect("no GPU adapter");
        log::info!("gpu: {}", adapter.get_info().name);
        // DMA-BUF import lets Chromium's frames reach us without a CPU copy.
        let zero_copy = cfg!(windows) || adapter.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: if zero_copy && cfg!(target_os = "linux") {
                    wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF
                } else {
                    wgpu::Features::empty()
                },
                ..Default::default()
            })
            .await
            .expect("request device");
        log::info!("zero-copy page frames: {zero_copy}");

        let caps = surface.get_capabilities(&adapter);
        // Non-sRGB target: we blend in sRGB space, exactly like page compositing does.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            desired_maximum_frame_latency: 2,
            // Never block in present: on Wayland a hidden window gets no frame callbacks and FIFO
            // would freeze the whole event loop. winit already paces redraws to the compositor's
            // frame callbacks (we call `pre_present_notify`), so mailbox stays vsync-paced.
            present_mode: if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
        };
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, format);
        Self { surface, device, queue, config, renderer }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn present(&mut self, window: &Window, scene: &mut Scene) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                frame
            }
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &view,
            (self.config.width, self.config.height),
            theme::BACKGROUND,
            scene,
        );
        window.pre_present_notify();
        self.queue.present(frame);
    }
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    /// Logical cursor position, for window resizing/moving from our own chrome.
    cursor: Option<(f32, f32)>,
    last_press: Option<Instant>,
    update_ready: bool,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    lock: Option<LockScreen>,
    vault: Option<Arc<vault::Vault>>,
    unlocking: Option<mpsc::Receiver<Outcome>>,
    /// Unlocked, waiting for the user to accept or skip the PIN upgrade.
    pending: Option<vault::Session>,
    session: Option<Arc<vault::Session>>,
    checkpoint: Option<JoinHandle<()>>,
    last_checkpoint: Instant,
    shield: Option<Arc<shield::Shield>>,
    engine_gpu: Option<engine::Gpu>,
    shell: Option<Shell>,
    scene: Scene,
    last_frame: Option<Instant>,
    animating: bool,
    engine_ready: bool,
    pump_at: Option<Instant>,
    last_pump: Instant,
}

impl App {
    fn redraw(&mut self) {
        let edge = self.edge();
        let (Some(window), Some(gpu)) = (&self.window, &mut self.gpu) else { return };
        let now = Instant::now();
        let dt = match self.last_frame {
            Some(last) => (now - last).as_secs_f32(),
            // First frame after idling: assume one refresh interval, not the idle gap.
            None => frame_interval(window),
        };
        self.last_frame = Some(now);

        let scale = window.scale_factor() as f32;
        let size: LogicalSize<f32> = window.inner_size().to_logical(scale as f64);
        self.scene.clear(scale);
        if let Some(shell) = &mut self.shell {
            self.animating = shell.frame(dt, size.width, size.height, scale, &mut self.scene);
            window.set_cursor(edge.map_or_else(|| shell.cursor_icon(), edge_cursor));
        } else if let Some(lock) = &mut self.lock {
            self.animating = lock.frame(dt, size.width, size.height, &mut self.scene);
        }
        gpu.present(window, &mut self.scene);

        if self.animating {
            window.request_redraw();
        } else {
            self.last_frame = None;
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn pump(&mut self) {
        if self.engine_ready {
            self.pump_at = None;
            self.last_pump = Instant::now();
            engine::pump();
        }
    }

    fn shutdown(&mut self) {
        if let Some(shell) = &mut self.shell {
            shell.close_all();
            // Let Chromium finish closing pages before tearing it down.
            let deadline = Instant::now() + Duration::from_secs(3);
            while !shell.all_closed() && Instant::now() < deadline {
                engine::pump();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.shell = None;
        if self.engine_ready {
            engine::shutdown();
            self.engine_ready = false;
        }
        // Closed while the PIN upgrade was on offer: Chromium never ran, just save and erase.
        if let Some(session) = self.pending.take()
            && let Err(e) = session.close() {
                log::error!("vault: final save failed, the session stays in RAM for recovery: {e}");
            }
        // Chromium has flushed and closed its files: now the final save is consistent.
        if let Some(handle) = self.checkpoint.take() {
            let _ = handle.join();
        }
        if let Some(session) = self.session.take() {
            match Arc::try_unwrap(session) {
                Ok(session) => match session.close() {
                    Ok(()) => log::info!("vault: saved and erased the RAM copy"),
                    Err(e) => log::error!("vault: final save failed, the session stays in RAM for recovery: {e}"),
                },
                Err(_) => log::error!("vault: session still in use at exit; it stays in RAM for recovery"),
            }
        }
    }

    /// The window edge under the cursor, if any (the browser draws no frame of its own).
    fn edge(&self) -> Option<ResizeDirection> {
        let (window, (x, y)) = (self.window.as_ref()?, self.cursor?);
        if window.is_maximized() {
            return None;
        }
        let size: LogicalSize<f32> = window.inner_size().to_logical(window.scale_factor());
        let (left, right) = (x < EDGE, x > size.width - EDGE);
        let (top, bottom) = (y < EDGE, y > size.height - EDGE);
        Some(match (left, right, top, bottom) {
            (true, _, true, _) => ResizeDirection::NorthWest,
            (_, true, true, _) => ResizeDirection::NorthEast,
            (true, _, _, true) => ResizeDirection::SouthWest,
            (_, true, _, true) => ResizeDirection::SouthEast,
            (true, ..) => ResizeDirection::West,
            (_, true, ..) => ResizeDirection::East,
            (_, _, true, _) => ResizeDirection::North,
            (_, _, _, true) => ResizeDirection::South,
            _ => return None,
        })
    }

    /// Edges resize, empty bar space moves, a double-click there maximizes. True if handled.
    fn window_control(&mut self) -> bool {
        let Some(window) = self.window.clone() else { return false };
        if let Some(edge) = self.edge() {
            let _ = window.drag_resize_window(edge);
            return true;
        }
        let in_bar = match (&self.shell, self.cursor) {
            (Some(shell), _) => shell.drag_area(),
            // Lock screen: the top strip.
            (None, Some((_, y))) => y < 60.0,
            (None, None) => false,
        };
        if !in_bar {
            return false;
        }
        let double = self.last_press.is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        self.last_press = Some(Instant::now());
        if double {
            window.set_maximized(!window.is_maximized());
        } else {
            let _ = window.drag_window();
        }
        true
    }

    /// Saves the session and replaces this process with the newly installed build.
    fn restart(&mut self, event_loop: &ActiveEventLoop) {
        let Some(exe) = update::restart_target() else { return };
        log::info!("restarting into the updated build");
        self.shutdown();
        let args: Vec<String> = std::env::args().skip(1).collect();
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Only returns on failure; the vault lock is released because exec closes our files.
            let error = std::process::Command::new(exe).args(args).exec();
            log::error!("restart failed: {error}");
        }
        #[cfg(windows)]
        if let Err(error) = std::process::Command::new(exe).args(args).spawn() {
            log::error!("restart failed: {error}");
        }
        event_loop.exit();
    }

    fn submit(&mut self, submit: Submit, event_loop: &ActiveEventLoop) {
        let Some(vault) = self.vault.clone() else { return };
        let job: Box<dyn FnOnce() -> Outcome + Send> = match submit {
            Submit::Create(pin) => Box::new(move || Outcome::Created(vault.create(&pin))),
            Submit::Unlock(pin) => Box::new(move || Outcome::Unlocked(vault.unlock(&pin))),
            Submit::Upgrade(pin) => {
                let Some(session) = self.pending.take() else { return };
                Box::new(move || {
                    let result = vault.upgrade_to_pin(&session, &pin);
                    Outcome::Upgraded(session, result)
                })
            }
            Submit::SkipUpgrade => {
                if let Some(session) = self.pending.take() {
                    self.start_browser(session, event_loop);
                }
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let outcome = job();
            log::info!("vault: done in {:?}", started.elapsed());
            let _ = tx.send(outcome);
            let _ = proxy.send_event(UserEvent::Unlocked);
        });
        self.unlocking = Some(rx);
    }

    fn unlocked(&mut self, event_loop: &ActiveEventLoop) {
        let Some(outcome) = self.unlocking.take().and_then(|rx| rx.try_recv().ok()) else { return };
        match outcome {
            Outcome::Created(Ok(session)) => {
                // Plaintext profile from before the vault existed.
                let _ = std::fs::remove_dir_all(data_dir().join("profile"));
                self.start_browser(session, event_loop);
            }
            Outcome::Unlocked(Ok(session)) => {
                if self.vault.as_ref().is_some_and(|v| v.can_upgrade()) {
                    self.pending = Some(session);
                    if let Some(lock) = &mut self.lock {
                        lock.offer_upgrade();
                    }
                } else {
                    self.start_browser(session, event_loop);
                }
            }
            Outcome::Upgraded(session, result) => {
                match result {
                    Ok(()) => log::info!("vault: now sealed in the TPM with a pin"),
                    Err(e) => log::warn!("vault: pin upgrade failed, keeping the passphrase: {e}"),
                }
                self.start_browser(session, event_loop);
            }
            Outcome::Created(Err(e)) | Outcome::Unlocked(Err(e)) => {
                log::warn!("vault: {e}");
                let creating = self.vault.as_ref().is_some_and(|v| matches!(v.state(), vault::State::New { .. }));
                if let Some(lock) = &mut self.lock {
                    lock.failed(&e, creating);
                }
            }
        }
        self.request_redraw();
    }

    /// After unlock: Chromium starts with its profile in the RAM session.
    fn start_browser(&mut self, session: vault::Session, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(shield), Some(engine_gpu)) = (self.window.clone(), self.shield.clone(), self.engine_gpu.clone())
        else {
            return;
        };
        shield.use_settings(session.settings_dir().join("shield.json"));

        let proxy = self.proxy.clone();
        let init_started = Instant::now();
        let started = engine::initialize(engine::Config {
            profile_dir: session.profile_dir(),
            shield: shield.clone(),
            schedule_pump: Arc::new(move |delay| {
                let _ = proxy.send_event(UserEvent::Pump(delay));
            }),
            wake: {
                let proxy = self.proxy.clone();
                Arc::new(move || {
                    let _ = proxy.send_event(UserEvent::Redraw);
                })
            },
        });
        let profile = profile::Profile::load(&session.settings_dir());
        self.session = Some(Arc::new(session));
        self.last_checkpoint = Instant::now();
        if let Err(e) = started {
            log::error!("{e}");
            self.shutdown();
            event_loop.exit();
            return;
        }
        self.engine_ready = true;
        log::info!("engine: chromium started in {:?}", init_started.elapsed());
        // Chromium installed its own SIGTERM handling during initialize; take it back.
        #[cfg(unix)]
        signals::reinstall();

        let proxy = self.proxy.clone();
        let vpn = vpn::watch(move || {
            let _ = proxy.send_event(UserEvent::Redraw);
        });
        let redraw_window = window.clone();
        let notify: Rc<dyn Fn()> = Rc::new(move || redraw_window.request_redraw());
        let proxy = self.proxy.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Redraw);
        });
        let Some(vault) = self.vault.clone() else { return };
        let host = host::Host::new(profile, vault, notify.clone());
        engine::set_host(host.clone());
        let mut shell = Shell::new(vpn, shield, engine_gpu, notify, data_dir().join("netinfo"), wake, host);
        // Dev hook: `UNSKILLED_OPEN="url1 url2"` opens pages at startup.
        if let Ok(urls) = std::env::var("UNSKILLED_OPEN") {
            shell.open_initial(urls.split_whitespace());
        }
        if std::env::var_os("UNSKILLED_PANEL").is_some() {
            shell.toggle_panel();
        }
        if self.update_ready {
            shell.set_update_ready();
        }
        if std::env::var_os("UNSKILLED_INSPECTOR").is_some() {
            shell.toggle_inspector();
        }
        if std::env::var_os("UNSKILLED_DEVTOOLS").is_some() {
            shell.open_devtools();
        }
        #[cfg(debug_assertions)]
        if let Ok(kind) = std::env::var("UNSKILLED_DEV_PROMPT") {
            shell.dev_prompt(&kind);
        }
        self.shell = Some(shell);
        self.lock = None;
    }

    fn maybe_checkpoint(&mut self) {
        if self.last_checkpoint.elapsed() < CHECKPOINT || self.checkpoint.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let Some(session) = self.session.clone() else { return };
        self.last_checkpoint = Instant::now();
        self.checkpoint = Some(std::thread::spawn(move || {
            if let Err(e) = session.save() {
                log::warn!("vault: checkpoint failed: {e}");
            }
        }));
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("unskilled")
            .with_inner_size(LogicalSize::new(1280.0, 820.0))
            .with_min_inner_size(LogicalSize::new(520.0, 360.0));
        // The app id the launcher entry and the compositor match on.
        #[cfg(target_os = "linux")]
        let attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(attributes, "unskilled-browser", "unskilled-browser");
        let window = Arc::new(event_loop.create_window(attributes).expect("create window"));
        let gpu = pollster::block_on(Gpu::new(window.clone()));
        let engine_gpu = engine::Gpu { device: gpu.device.clone(), queue: gpu.queue.clone() };
        self.gpu = Some(gpu);

        let proxy = self.proxy.clone();
        update::watch(move || {
            let _ = proxy.send_event(UserEvent::UpdateReady);
        });

        let data = data_dir();
        // Filter lists load while the PIN is typed, so protection is ready at unlock.
        let proxy = self.proxy.clone();
        let shield = shield::Shield::new(data.join("shield"), move || {
            let _ = proxy.send_event(UserEvent::Redraw);
        });
        self.shield = Some(shield);
        self.engine_gpu = Some(engine_gpu);

        let vault = match vault::Vault::open(data.join("vault")) {
            Ok(vault) => Arc::new(vault),
            Err(e) => {
                log::error!("{e}");
                event_loop.exit();
                return;
            }
        };
        self.lock = Some(LockScreen::new(vault.state(), vault.tpm_denied()));
        self.vault = Some(vault);

        // Dev builds only: `UNSKILLED_DEV_PIN` types the PIN, for automated runs.
        #[cfg(debug_assertions)]
        if let Ok(pin) = std::env::var("UNSKILLED_DEV_PIN") {
            let pin = zeroize::Zeroizing::new(pin);
            let submit = match self.lock.as_ref().map(|_| self.vault.as_ref().unwrap().state()) {
                Some(vault::State::New { .. }) => Submit::Create(pin),
                _ => Submit::Unlock(pin),
            };
            self.submit(submit, event_loop);
        }
        window.request_redraw();
        self.window = Some(window);
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        let now = Instant::now();
        if self.pump_at.is_some_and(|t| t <= now) || now - self.last_pump >= PUMP_FALLBACK {
            self.pump();
        }
        if let StartCause::ResumeTimeReached { .. } = cause {
            // Clock tick on the start page.
            self.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Redraw => self.request_redraw(),
            UserEvent::Unlocked => self.unlocked(_event_loop),
            UserEvent::UpdateReady => {
                self.update_ready = true;
                if let Some(shell) = &mut self.shell {
                    shell.set_update_ready();
                }
                // Dev builds only: exercise the restart path without a click.
                #[cfg(debug_assertions)]
                if std::env::var_os("UNSKILLED_DEV_AUTORESTART").is_some() {
                    self.restart(_event_loop);
                    return;
                }
                self.request_redraw();
            }
            UserEvent::Quit => {
                log::info!("signal received, shutting down");
                self.shutdown();
                _event_loop.exit();
            }
            UserEvent::Pump(delay) if delay <= 0 => self.pump(),
            UserEvent::Pump(delay) => {
                let at = Instant::now() + Duration::from_millis(delay as u64);
                self.pump_at = Some(self.pump_at.map_or(at, |t| t.min(at)));
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
        match event {
            WindowEvent::CloseRequested => {
                self.shutdown();
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.request_redraw(),
            WindowEvent::ModifiersChanged(modifiers) => {
                if let Some(shell) = &mut self.shell {
                    shell.set_modifiers(modifiers.state());
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(shell) = &mut self.shell {
                    shell.key(&event);
                } else if let Some(submit) = self.lock.as_mut().and_then(|lock| lock.key(&event)) {
                    self.submit(submit, event_loop);
                }
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = position.to_logical::<f32>(scale);
                self.cursor = Some((p.x, p.y));
                if let Some(shell) = &mut self.shell {
                    shell.cursor_moved(p.x, p.y);
                }
                if let (Some(window), Some(edge)) = (&self.window, self.edge()) {
                    window.set_cursor(edge_cursor(edge));
                }
                self.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(shell) = &mut self.shell {
                    shell.cursor_left();
                }
                self.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => engine::Button::Left,
                    MouseButton::Middle => engine::Button::Middle,
                    MouseButton::Right => engine::Button::Right,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                if pressed && button == engine::Button::Left && self.window_control() {
                    return;
                }
                if let Some(shell) = &mut self.shell {
                    shell.mouse_button(button, pressed);
                    if shell.take_restart_request() {
                        self.restart(event_loop);
                        return;
                    }
                }
                self.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy, precise) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * LINE, y * LINE, false),
                    MouseScrollDelta::PixelDelta(p) => {
                        let p = p.to_logical::<f32>(scale);
                        (p.x, p.y, true)
                    }
                };
                if let Some(shell) = &mut self.shell {
                    shell.wheel(dx, dy, precise);
                }
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.maybe_checkpoint();
        let now = Instant::now();
        // Chromium's pump only matters once it's running (after unlock).
        let mut wake = if self.engine_ready { self.last_pump + PUMP_FALLBACK } else { now + Duration::from_secs(60) };
        if let Some(at) = self.pump_at {
            wake = wake.min(at);
        }
        if !self.animating {
            // Start page clock: wake when the minute changes.
            let until_minute = 60 - clock::now().second.clamp(0, 59) as u64;
            wake = wake.min(now + Duration::from_secs(until_minute));
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake.max(now)));
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown();
    }
}

fn frame_interval(window: &Window) -> f32 {
    let millihertz = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).unwrap_or(60_000);
    1000.0 / millihertz as f32
}

/// `%LOCALAPPDATA%\unskilled-browser`.
#[cfg(windows)]
fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("unskilled-browser")
}

/// `$XDG_DATA_HOME/unskilled-browser`, falling back to `~/.local/share/unskilled-browser`.
#[cfg(not(windows))]
fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("unskilled-browser")
}

fn main() {
    // Chromium relaunches this binary for its renderer/GPU/utility processes.
    if let Some(code) = engine::subprocess() {
        std::process::exit(code);
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn"))
        .init();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    #[cfg(unix)]
    {
        let proxy = event_loop.create_proxy();
        signals::install(move || {
            let _ = proxy.send_event(UserEvent::Quit);
        });
    }
    let mut app = App {
        proxy: event_loop.create_proxy(),
        cursor: None,
        last_press: None,
        update_ready: false,
        window: None,
        gpu: None,
        lock: None,
        vault: None,
        unlocking: None,
        pending: None,
        session: None,
        checkpoint: None,
        last_checkpoint: Instant::now(),
        shield: None,
        engine_gpu: None,
        shell: None,
        scene: Scene::new(1.0),
        last_frame: None,
        animating: false,
        engine_ready: false,
        pump_at: None,
        last_pump: Instant::now(),
    };
    event_loop.run_app(&mut app).expect("run");
}

fn edge_cursor(edge: ResizeDirection) -> CursorIcon {
    match edge {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
    }
}
