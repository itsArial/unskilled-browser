//! One web page: a windowless CEF browser rendering into GPU textures we composite ourselves.

use crate::{
    devtools::{Bridge, Wake},
    inspect::{self, Inspect},
    net::{self, SharedNet},
    renderer::{MSG_FILL, MSG_LOGIN, MSG_QUERY, MSG_REPLY, MSG_WEBUI},
    webui,
};
use cef::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Text,
    Wait,
    Progress,
    Crosshair,
    Move,
    NotAllowed,
    Grab,
    ResizeEw,
    ResizeNs,
    ResizeNesw,
    ResizeNwse,
    Help,
}

/// What the shell reads to draw the tab.
#[derive(Default)]
pub struct TabState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Load progress, 0 → 1, while `loading`.
    pub progress: f32,
    pub cursor: Cursor,
    /// Latest rendered frame, physical pixels.
    pub frame: Option<wgpu::Texture>,
    frame_at: Option<std::time::Instant>,
    /// A repaint is due once frames stop arriving (see `Tab::begin_frame`).
    needs_settle: bool,
    /// The forced repaint is in flight; its frame must not schedule another.
    settling: bool,
    /// Pages asked to open these (window.open, target=_blank); the shell opens tabs for them.
    pub open_requests: Vec<String>,
    /// Popups Chromium created for this page (kept connected to it, so sign-in pop-ups can report
    /// back through `window.opener`); the shell adopts them as tabs.
    pub popups: Vec<Tab>,
    /// A right-click menu waiting for the shell to show it.
    pub context_menu: Option<ContextMenu>,
    pub closed: bool,
}

/// Our own commands added to Chromium's context menu.
pub mod commands {
    pub const OPEN_LINK_IN_TAB: i32 = 26501;
    pub const COPY_LINK: i32 = 26502;
    pub const INSPECT: i32 = 26503;
}

pub struct MenuItem {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
}

/// Chromium's context menu, drawn by the shell. Answer with `Tab::finish_context_menu`.
pub struct ContextMenu {
    /// View coordinates (logical pixels) of the click.
    pub x: f32,
    pub y: f32,
    pub items: Vec<MenuItem>,
    pub link: Option<String>,
    callback: RunContextMenuCallback,
}

/// Viewport the page lays out into.
struct View {
    width: Cell<f32>,
    height: Cell<f32>,
    scale: Cell<f32>,
}

#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

pub struct Tab {
    /// Filled at creation, or when Chromium creates it for us (DevTools).
    browser: Rc<RefCell<Option<Browser>>>,
    pub state: Rc<RefCell<TabState>>,
    pub net: SharedNet,
    pub inspect: Rc<RefCell<Inspect>>,
    view: Rc<View>,
    watcher: Option<Registration>,
    /// Set on a DevTools pane: its connection to the inspected page.
    bridge: Option<Bridge>,
}

fn window_info() -> WindowInfo {
    WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: shared_textures() as _,
        // Shared textures are only produced when we drive Chromium's frame clock.
        external_begin_frame_enabled: shared_textures() as _,
        runtime_style: RuntimeStyle::ALLOY,
        ..Default::default()
    }
}

fn browser_settings() -> BrowserSettings {
    BrowserSettings { windowless_frame_rate: 60, background_color: 0xFFFFFFFF, ..Default::default() }
}

/// Mouse buttons we forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Down,
    Up,
    Char,
}

#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub kind: KeyKind,
    pub windows_code: i32,
    pub native_code: i32,
    pub character: u16,
    pub modifiers: u32,
}

/// Modifier bits as CEF expects them.
pub mod modifiers {
    pub const SHIFT: u32 = 1 << 1;
    pub const CONTROL: u32 = 1 << 2;
    pub const ALT: u32 = 1 << 3;
    pub const LEFT_BUTTON: u32 = 1 << 4;
    pub const MIDDLE_BUTTON: u32 = 1 << 5;
    pub const RIGHT_BUTTON: u32 = 1 << 6;
    pub const COMMAND: u32 = 1 << 7;
    pub const PRECISION_SCROLL: u32 = 1 << 14;
}

impl Tab {
    /// Shared state + a client wired to it. The browser itself is attached by the caller.
    fn parts(url: &str, gpu: Gpu, width: f32, height: f32, scale: f32, notify: Rc<dyn Fn()>) -> (Self, Client) {
        let state = Rc::new(RefCell::new(TabState { url: url.to_owned(), ..Default::default() }));
        let view = Rc::new(View { width: Cell::new(width), height: Cell::new(height), scale: Cell::new(scale) });
        let net: SharedNet = Arc::new(Mutex::new(net::TabNet::default()));
        let inspect = Rc::new(RefCell::new(Inspect::default()));
        let browser = Rc::new(RefCell::new(None));

        let handlers = Handlers {
            state: state.clone(),
            view: view.clone(),
            gpu,
            notify,
            net: net.clone(),
            inspect: inspect.clone(),
            browser: browser.clone(),
            watcher: Rc::new(RefCell::new(None)),
        };
        let client = PageClient::new(
            Paint::new(handlers.clone()),
            Display::new(handlers.clone()),
            Load::new(handlers.clone()),
            Life::new(handlers.clone()),
            Requests::new(handlers.clone()),
            Dialogs::new(),
            Menus::new(handlers),
        );
        (Self { browser, state, net, inspect, view, watcher: None, bridge: None }, client)
    }

    pub fn create(url: &str, gpu: Gpu, width: f32, height: f32, scale: f32, notify: Rc<dyn Fn()>) -> Option<Self> {
        Self::create_with(url, gpu, width, height, scale, notify, true)
    }

    fn create_with(
        url: &str,
        gpu: Gpu,
        width: f32,
        height: f32,
        scale: f32,
        notify: Rc<dyn Fn()>,
        watch: bool,
    ) -> Option<Self> {
        let (mut tab, mut client) = Self::parts(url, gpu, width, height, scale, notify.clone());
        // Start blank and navigate once the network watcher is attached, so the inspector sees the
        // page's own document request too.
        let browser = browser_host_create_browser_sync(
            Some(&window_info()),
            Some(&mut client),
            Some(&CefString::from("about:blank")),
            Some(&browser_settings()),
            None,
            None,
        )?;
        net::register_tab(browser.identifier(), tab.net.clone());
        if watch {
            tab.watcher = inspect::watch(&browser, tab.inspect.clone(), notify);
        }
        *tab.browser.borrow_mut() = Some(browser);
        tab.navigate(url);
        Some(tab)
    }

    /// Chrome DevTools for this page, as a pane of its own: the DevTools frontend runs as an
    /// ordinary windowless page connected to this one (see `devtools`). `wake` redraws when the
    /// frontend sends commands.
    pub fn open_devtools(&self, gpu: Gpu, width: f32, height: f32, scale: f32, notify: Rc<dyn Fn()>, wake: Wake) -> Option<Self> {
        let bridge = Bridge::start(&self.browser()?, wake)?;
        let mut tab = Self::create_with(&bridge.frontend, gpu, width, height, scale, notify, false)?;
        tab.bridge = Some(bridge);
        Some(tab)
    }

    fn browser(&self) -> Option<Browser> {
        self.browser.borrow().clone()
    }

    /// Chromium's id for this page (matches what `webui::Host` callbacks receive).
    pub fn browser_id(&self) -> Option<i32> {
        self.browser().map(|b| b.identifier())
    }

    /// The sign-in prompt waiting on this page, if any: `(site, provider id, provider name)`.
    pub fn identity_prompt(&self) -> Option<(String, &'static str, &'static str)> {
        let net = self.net.lock().unwrap();
        net.identity_prompt.as_ref().map(|p| (p.site.clone(), p.provider, p.provider_name))
    }

    /// Resumes the paused sign-in, with (`share`) or without the provider's cookies.
    pub fn answer_identity(&self, share: bool) {
        let prompt = self.net.lock().unwrap().identity_prompt.take();
        if let Some(prompt) = prompt {
            prompt.answer(share);
        }
    }

    /// Fills a login into the frame that showed the form.
    pub fn fill_login(&self, frame: &str, username: &str, password: &str) {
        let Some(frame) = self.browser().and_then(|b| b.frame_by_identifier(Some(&CefString::from(frame)))) else { return };
        let Some(mut message) = process_message_create(Some(&CefString::from(MSG_FILL))) else { return };
        if let Some(list) = message.argument_list() {
            list.set_string(0, Some(&CefString::from(username)));
            list.set_string(1, Some(&CefString::from(password)));
        }
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
    }

    fn host(&self) -> Option<BrowserHost> {
        self.browser()?.host()
    }

    pub fn navigate(&self, url: &str) {
        if let Some(frame) = self.browser().and_then(|b| b.main_frame()) {
            frame.load_url(Some(&CefString::from(url)));
        }
    }

    pub fn back(&self) {
        if let Some(b) = self.browser() {
            b.go_back();
        }
    }

    pub fn forward(&self) {
        if let Some(b) = self.browser() {
            b.go_forward();
        }
    }

    pub fn reload(&self) {
        if let Some(b) = self.browser() {
            b.reload();
        }
    }

    pub fn stop(&self) {
        if let Some(b) = self.browser() {
            b.stop_load();
        }
    }

    /// Runs the chosen context-menu command (`None` = dismissed). Our own commands are handled
    /// by the shell first and arrive here as `None`.
    pub fn finish_context_menu(&self, command: Option<i32>) {
        let Some(menu) = self.state.borrow_mut().context_menu.take() else { return };
        match command {
            Some(id) if id < commands::OPEN_LINK_IN_TAB => menu.callback.cont(id, EventFlags::default()),
            _ => menu.callback.cancel(),
        }
    }

    pub fn resize(&self, width: f32, height: f32, scale: f32) {
        let changed_scale = self.view.scale.get() != scale;
        if self.view.width.get() == width && self.view.height.get() == height && !changed_scale {
            return;
        }
        self.view.width.set(width);
        self.view.height.set(height);
        self.view.scale.set(scale);
        if let Some(host) = self.host() {
            if changed_scale {
                host.notify_screen_info_changed();
            }
            host.was_resized();
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if let Some(host) = self.host() {
            host.was_hidden((!visible) as _);
        }
    }

    pub fn focus(&self, focused: bool) {
        if let Some(host) = self.host() {
            host.set_focus(focused as _);
        }
    }

    pub fn mouse_move(&self, x: f32, y: f32, modifiers: u32, leave: bool) {
        if let Some(host) = self.host() {
            host.send_mouse_move_event(Some(&mouse(x, y, modifiers)), leave as _);
        }
    }

    pub fn mouse_button(&self, x: f32, y: f32, button: Button, up: bool, clicks: i32, modifiers: u32) {
        let button = match button {
            Button::Left => MouseButtonType::LEFT,
            Button::Middle => MouseButtonType::MIDDLE,
            Button::Right => MouseButtonType::RIGHT,
        };
        if let Some(host) = self.host() {
            host.send_mouse_click_event(Some(&mouse(x, y, modifiers)), button, up as _, clicks);
        }
    }

    pub fn wheel(&self, x: f32, y: f32, dx: f32, dy: f32, modifiers: u32) {
        if let Some(host) = self.host() {
            host.send_mouse_wheel_event(Some(&mouse(x, y, modifiers)), dx.round() as _, dy.round() as _);
        }
    }

    pub fn key(&self, key: Key) {
        let event = KeyEvent {
            type_: match key.kind {
                KeyKind::Down => KeyEventType::RAWKEYDOWN,
                KeyKind::Up => KeyEventType::KEYUP,
                KeyKind::Char => KeyEventType::CHAR,
            },
            modifiers: key.modifiers,
            windows_key_code: key.windows_code,
            native_key_code: key.native_code,
            character: key.character,
            unmodified_character: key.character,
            ..Default::default()
        };
        if let Some(host) = self.host() {
            host.send_key_event(Some(&event));
        }
    }

    /// Asks Chromium for a frame (vsync-driven when zero-copy frames are on).
    pub fn begin_frame(&self) {
        if let Some(bridge) = &self.bridge {
            bridge.pump();
        }
        if !shared_textures() {
            return;
        }
        let Some(host) = self.host() else { return };
        // Chromium can hand over a static page's last frame before it finished painting and then
        // send nothing more; once frames go quiet, force one full repaint.
        let settle = {
            let mut state = self.state.borrow_mut();
            let quiet = state.frame_at.is_some_and(|t| t.elapsed() > std::time::Duration::from_millis(120));
            let due = state.needs_settle && quiet;
            if due {
                state.needs_settle = false;
                state.settling = true;
            }
            due
        };
        if settle {
            host.invalidate(PaintElementType::VIEW);
        }
        host.send_external_begin_frame();
    }

    pub fn close(&self) {
        self.finish_context_menu(None);
        self.answer_identity(false);
        if let Some(browser) = self.browser() {
            net::unregister_tab(browser.identifier());
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
        }
    }
}

/// Zero-copy GPU frames unless `UNSKILLED_SOFTWARE_PAINT` is set.
fn shared_textures() -> bool {
    std::env::var_os("UNSKILLED_SOFTWARE_PAINT").is_none()
}

/// Blocks until the GPU work writing this DMA-BUF has finished. Vulkan imports don't honour
/// the buffer's implicit fences, so without this we can copy a half-rendered frame.
#[cfg(target_os = "linux")]
fn wait_for_writes(fd: i32) {
    #[repr(C)]
    struct ExportSyncFile {
        flags: u32,
        fd: i32,
    }
    // _IOWR('b', 2, struct dma_buf_export_sync_file)
    const DMA_BUF_IOCTL_EXPORT_SYNC_FILE: libc::c_ulong = 0xC008_6202;
    const DMA_BUF_SYNC_READ: u32 = 1;
    const TIMEOUT_MS: i32 = 50;

    let mut arg = ExportSyncFile { flags: DMA_BUF_SYNC_READ, fd: -1 };
    // SAFETY: plain ioctl/poll/close on file descriptors; `arg` matches the kernel struct.
    unsafe {
        if libc::ioctl(fd, DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &mut arg) != 0 || arg.fd < 0 {
            return;
        }
        let mut pfd = libc::pollfd { fd: arg.fd, events: libc::POLLIN, revents: 0 };
        libc::poll(&mut pfd, 1, TIMEOUT_MS);
        libc::close(arg.fd);
    }
}

fn mouse(x: f32, y: f32, modifiers: u32) -> MouseEvent {
    MouseEvent { x: x.round() as _, y: y.round() as _, modifiers }
}

#[derive(Clone)]
struct Handlers {
    state: Rc<RefCell<TabState>>,
    view: Rc<View>,
    gpu: Gpu,
    notify: Rc<dyn Fn()>,
    net: SharedNet,
    inspect: Rc<RefCell<Inspect>>,
    browser: Rc<RefCell<Option<Browser>>>,
    /// Network watcher of a popup, registered once Chromium has created it.
    watcher: Rc<RefCell<Option<Registration>>>,
}

wrap_client! {
    struct PageClient {
        paint: RenderHandler,
        display: DisplayHandler,
        load: LoadHandler,
        life: LifeSpanHandler,
        requests: RequestHandler,
        dialogs: JsdialogHandler,
        menus: ContextMenuHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.paint.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.requests.clone())
        }

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.dialogs.clone())
        }

        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.menus.clone())
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let (Some(frame), Some(message)) = (frame, message) else { return 0 };
            let name = CefString::from(&message.name()).to_string();
            let frame_url = CefString::from(&frame.url()).to_string();
            if name == MSG_WEBUI {
                // Only our own pages may use the bridge — checked here, in the browser process.
                if !frame_url.starts_with("unskilled://") {
                    log::warn!("engine: bridge call from {frame_url} refused");
                    return 1;
                }
                let (Some(args), Some(host)) = (message.argument_list(), webui::host()) else { return 1 };
                let id: i32 = CefString::from(&args.string(0)).to_string().parse().unwrap_or(0);
                let method = CefString::from(&args.string(1)).to_string();
                let params = CefString::from(&args.string(2)).to_string();
                let reply_json = host.webui(&method, &params);
                if let Some(mut reply) = process_message_create(Some(&CefString::from(MSG_REPLY))) {
                    if let Some(list) = reply.argument_list() {
                        list.set_int(0, id);
                        list.set_string(1, Some(&CefString::from(reply_json.as_str())));
                    }
                    frame.send_process_message(ProcessId::RENDERER, Some(&mut reply));
                }
                return 1;
            }
            if name == MSG_LOGIN {
                let (Some(args), Some(host), Some(browser)) = (message.argument_list(), webui::host(), _browser) else { return 1 };
                if !frame_url.starts_with("https://") && !frame_url.starts_with("http://") {
                    return 1;
                }
                let kind = CefString::from(&args.string(0)).to_string();
                if kind == "form" {
                    host.login_form(browser.identifier(), &frame_url, &CefString::from(&frame.identifier()).to_string());
                } else if kind == "submit" {
                    let json: serde_json::Value = serde_json::from_str(&CefString::from(&args.string(1)).to_string()).unwrap_or_default();
                    host.login_submitted(
                        browser.identifier(),
                        &frame_url,
                        json["username"].as_str().unwrap_or_default(),
                        json["password"].as_str().unwrap_or_default(),
                    );
                }
                return 1;
            }
            if name != MSG_QUERY {
                return 0;
            }
            let Some(args) = message.argument_list() else { return 1 };
            let id = args.int(0);
            let frame_url = CefString::from(&args.string(1)).to_string();
            let classes: Vec<String> = serde_json::from_str(&CefString::from(&args.string(2)).to_string()).unwrap_or_default();
            let ids: Vec<String> = serde_json::from_str(&CefString::from(&args.string(3)).to_string()).unwrap_or_default();
            let css = net::generic_selectors(&frame_url, &classes, &ids);
            if let Some(mut reply) = process_message_create(Some(&CefString::from(MSG_REPLY))) {
                if let Some(list) = reply.argument_list() {
                    list.set_int(0, id);
                    list.set_string(1, Some(&CefString::from(css.as_str())));
                }
                frame.send_process_message(ProcessId::RENDERER, Some(&mut reply));
            }
            1
        }
    }
}

wrap_render_handler! {
    struct Paint {
        h: Handlers,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = (self.h.view.width.get().round() as i32).max(1);
                rect.height = (self.h.view.height.get().round() as i32).max(1);
            }
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            let Some(info) = info else { return 0 };
            info.device_scale_factor = self.h.view.scale.get();
            1
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            use cef::osr_texture_import::SharedTextureHandle;
            let Some(info) = info else { return };
            if type_ != PaintElementType::default() {
                return;
            }
            #[cfg(target_os = "linux")]
            for plane in info.planes.iter().take(info.plane_count as usize) {
                wait_for_writes(plane.fd);
            }
            // SAFETY: called inside CEF's paint callback for the current frame; our wgpu device
            // runs on the same GPU and Vulkan driver as Chromium's GPU process.
            match unsafe { SharedTextureHandle::new(info).import_texture(&self.h.gpu.device, &self.h.gpu.queue) } {
                Ok(texture) => {
                    log::trace!("engine: accelerated frame {:?}", texture.size());
                    let mut state = self.h.state.borrow_mut();
                    state.frame = Some(texture);
                    state.frame_at = Some(std::time::Instant::now());
                    state.needs_settle = !std::mem::take(&mut state.settling);
                    drop(state);
                    (self.h.notify)();
                }
                Err(e) => log::warn!("engine: importing page frame failed: {e:?}"),
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            // Software fallback when shared textures are unavailable.
            if type_ != PaintElementType::default() || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let (w, h) = (width as u32, height as u32);
            log::trace!("engine: software frame {w}x{h}");
            // SAFETY: CEF provides a BGRA buffer of width * height * 4 bytes for this callback.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, (w * h * 4) as usize) };
            let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
            let reuse = self.h.state.borrow().frame.as_ref().is_some_and(|t| t.size() == size);
            if !reuse {
                let texture = self.h.gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("page frame"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                self.h.state.borrow_mut().frame = Some(texture);
            }
            let state = self.h.state.borrow();
            let texture = state.frame.as_ref().unwrap();
            self.h.gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
                size,
            );
            drop(state);
            (self.h.notify)();
        }
    }
}

wrap_display_handler! {
    struct Display {
        h: Handlers,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() != 0) {
                let url = url.map(|u| u.to_string()).unwrap_or_default();
                // Pages served straight from a service worker cache never hit the network layer.
                self.h.net.lock().unwrap().top_url = url.clone();
                self.h.state.borrow_mut().url = url;
                (self.h.notify)();
            }
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.h.state.borrow_mut().progress = progress as f32;
            (self.h.notify)();
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            self.h.state.borrow_mut().title = title.map(|t| t.to_string()).unwrap_or_default();
            (self.h.notify)();
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: ::std::os::raw::c_ulong,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            let cursor = match type_ {
                CursorType::HAND => Cursor::Pointer,
                CursorType::IBEAM | CursorType::VERTICALTEXT => Cursor::Text,
                CursorType::WAIT => Cursor::Wait,
                CursorType::PROGRESS => Cursor::Progress,
                CursorType::CROSS | CursorType::CELL => Cursor::Crosshair,
                CursorType::MOVE => Cursor::Move,
                CursorType::NOTALLOWED | CursorType::NODROP => Cursor::NotAllowed,
                CursorType::GRAB | CursorType::GRABBING => Cursor::Grab,
                CursorType::EASTRESIZE | CursorType::WESTRESIZE | CursorType::EASTWESTRESIZE | CursorType::COLUMNRESIZE => Cursor::ResizeEw,
                CursorType::NORTHRESIZE | CursorType::SOUTHRESIZE | CursorType::NORTHSOUTHRESIZE | CursorType::ROWRESIZE => Cursor::ResizeNs,
                CursorType::NORTHEASTRESIZE | CursorType::SOUTHWESTRESIZE | CursorType::NORTHEASTSOUTHWESTRESIZE => Cursor::ResizeNesw,
                CursorType::NORTHWESTRESIZE | CursorType::SOUTHEASTRESIZE | CursorType::NORTHWESTSOUTHEASTRESIZE => Cursor::ResizeNwse,
                CursorType::HELP => Cursor::Help,
                _ => Cursor::Default,
            };
            self.h.state.borrow_mut().cursor = cursor;
            (self.h.notify)();
            1
        }
    }
}

wrap_load_handler! {
    struct Load {
        h: Handlers,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            let mut state = self.h.state.borrow_mut();
            state.loading = is_loading != 0;
            state.can_go_back = can_go_back != 0;
            state.can_go_forward = can_go_forward != 0;
            drop(state);
            (self.h.notify)();
        }
    }
}

wrap_life_span_handler! {
    struct Life {
        h: Handlers,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            window_info: Option<&mut WindowInfo>,
            client: Option<&mut Option<Client>>,
            settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let url = target_url.map(|u| u.to_string()).unwrap_or_default();
            let (Some(window_info), Some(client), Some(settings)) = (window_info, client, settings) else {
                // Can't host it ourselves: fall back to a plain new tab.
                if !url.is_empty() {
                    self.h.state.borrow_mut().open_requests.push(url);
                    (self.h.notify)();
                }
                return 1;
            };
            // Let Chromium create the popup — windowless, rendered by us, shown as a tab — so it
            // stays connected to its opener (sign-in pop-ups report back through it).
            let (width, height, scale) = (self.h.view.width.get(), self.h.view.height.get(), self.h.view.scale.get());
            let (popup, popup_client) = Tab::parts(&url, self.h.gpu.clone(), width, height, scale, self.h.notify.clone());
            *window_info = self::window_info();
            *settings = browser_settings();
            *client = Some(popup_client);
            self.h.state.borrow_mut().popups.push(popup);
            (self.h.notify)();
            0
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            // Browsers Chromium creates for us (popups) arrive here; ours are already in the slot.
            let Some(browser) = browser else { return };
            let mut slot = self.h.browser.borrow_mut();
            if slot.is_none() {
                net::register_tab(browser.identifier(), self.h.net.clone());
                *self.h.watcher.borrow_mut() = inspect::watch(browser, self.h.inspect.clone(), self.h.notify.clone());
                *slot = Some(browser.clone());
            }
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let mut state = self.h.state.borrow_mut();
            state.closed = true;
            state.frame = None;
        }
    }
}

wrap_request_handler! {
    struct Requests {
        h: Handlers,
    }

    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _user_gesture: ::std::os::raw::c_int,
            is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if is_redirect == 0 && frame.is_some_and(|f| f.is_main() != 0) {
                self.h.inspect.borrow_mut().reset();
            }
            0
        }

        fn resource_request_handler(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _is_navigation: ::std::os::raw::c_int,
            _is_download: ::std::os::raw::c_int,
            request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut ::std::os::raw::c_int>,
        ) -> Option<ResourceRequestHandler> {
            let initiator = request_initiator.map(|i| i.to_string()).filter(|i| !i.is_empty() && i != "null");
            Some(net::interceptor(browser, initiator))
        }
    }
}

wrap_jsdialog_handler! {
    struct Dialogs;

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            _dialog_type: JsdialogType,
            _message_text: Option<&CefString>,
            _default_prompt_text: Option<&CefString>,
            _callback: Option<&mut JsdialogCallback>,
            suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // No native dialogs in windowless mode; our own dialog UI comes later.
            if let Some(suppress) = suppress_message {
                *suppress = 1;
            }
            0
        }
    }
}

wrap_context_menu_handler! {
    struct Menus {
        h: Handlers,
    }

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            let (Some(params), Some(model)) = (params, model) else { return };
            let link = CefString::from(&params.link_url()).to_string();
            if !link.is_empty() {
                if model.count() > 0 {
                    model.add_separator();
                }
                model.add_item(commands::OPEN_LINK_IN_TAB, Some(&CefString::from("Open link in new tab")));
                model.add_item(commands::COPY_LINK, Some(&CefString::from("Copy link address")));
            }
            if model.count() > 0 {
                model.add_separator();
            }
            model.add_item(commands::INSPECT, Some(&CefString::from("Inspect")));
        }

        fn run_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
            callback: Option<&mut RunContextMenuCallback>,
        ) -> ::std::os::raw::c_int {
            let (Some(params), Some(model), Some(callback)) = (params, model, callback) else { return 0 };
            let mut items: Vec<MenuItem> = (0..model.count())
                .filter(|&i| model.is_visible_at(i) != 0)
                .map(|i| MenuItem {
                    id: model.command_id_at(i),
                    // Chromium labels carry `&` accelerator marks.
                    label: CefString::from(&model.label_at(i)).to_string().replace('&', ""),
                    enabled: model.is_enabled_at(i) != 0,
                    separator: model.type_at(i) == MenuItemType::SEPARATOR,
                })
                .filter(|item| item.separator || !item.label.is_empty())
                .collect();
            // No leading, trailing or doubled separators.
            items.dedup_by(|a, b| a.separator && b.separator);
            while items.first().is_some_and(|i| i.separator) {
                items.remove(0);
            }
            while items.last().is_some_and(|i| i.separator) {
                items.pop();
            }
            let link = CefString::from(&params.link_url()).to_string();
            let previous = self.h.state.borrow_mut().context_menu.replace(ContextMenu {
                x: params.xcoord() as f32,
                y: params.ycoord() as f32,
                items,
                link: (!link.is_empty()).then_some(link),
                callback: callback.clone(),
            });
            if let Some(previous) = previous {
                previous.callback.cancel();
            }
            (self.h.notify)();
            1
        }
    }
}
