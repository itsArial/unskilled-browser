//! The browser chrome: top bar, tab strip, content card and panels. State, input routing and
//! drawing; the window/GPU glue lives in `main.rs`.

use crate::{
    clipboard::{copy, paste},
    clock,
    host::{Event, Host},
    inspector::{HostRow, Inspector, InspectorData, InspectorHit, SiteInfo},
    keys,
    menu::{self, MenuHit, MenuView, Row},
    panel::{Panel, PanelData, PanelHit},
    sheet::{Input, SheetHit, SheetView, Spec},
    theme,
    vpn::VpnStatus,
};
use netinfo::NetInfo;
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Instant,
};
use zeroize::Zeroizing;
use ui::{
    Color, Rect, Scene, Spring, SpringConfig,
    Weight,
};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::CursorIcon,
};

const MARGIN: f32 = 14.0;
const BAR_Y: f32 = 14.0;
const BAR_H: f32 = 40.0;
const TABS_Y: f32 = BAR_Y + BAR_H + 12.0;
const TAB_H: f32 = 32.0;
const TAB_W: f32 = 184.0;
const TAB_GAP: f32 = 8.0;
const CONTENT_Y: f32 = TABS_Y + TAB_H + 12.0;
const CONTENT_RADIUS: f32 = 18.0;
/// JetBrains Mono sizes, matching the terminal and bar.
const UI_TEXT: f32 = 13.0;
const TAB_TEXT: f32 = 12.5;
const SMALL_TEXT: f32 = 11.0;
const LOGO: f32 = 22.0;
const SETTINGS_URL: &str = "unskilled://settings";
const BOOKMARKS_URL: &str = "unskilled://bookmarks";
/// Most bookmarks the start page shows.
const TILES: usize = 8;
const TILE_W: f32 = 176.0;
const TILE_H: f32 = 52.0;

/// Dot-matrix icons, Nothing-style: `#` is a dot.
const RIBBON: [&str; 6] = ["#####", "#...#", "#...#", "#...#", "#.#.#", "##.##"];
const RIBBON_FILLED: [&str; 6] = ["#####", "#####", "#####", "#####", "##.##", "#...#"];
const KEY: [&str; 3] = [".#.....", "#.#####", ".#...#."];

struct Tab {
    id: u64,
    /// `None` until the first navigation: the tab shows the start page.
    page: Option<engine::Tab>,
    /// Chrome DevTools docked next to the page.
    devtools: Option<engine::Tab>,
    /// 0 → 1 as the tab opens, back to 0 when it closes. Drives width, gap and opacity.
    presence: Spring,
    hover: Spring,
    closing: bool,
    /// Pages left behind in other site sandboxes, for back/forward across them.
    back: Vec<String>,
    forward: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Back,
    Forward,
    /// A new navigation into another sandbox.
    New,
}

#[derive(Clone, Copy)]
enum AppAction {
    NewTab,
    Bookmark,
    Bookmarks,
    Settings,
    DevTools,
    Inspector,
    Restart,
    None,
}

enum Current {
    Identity { browser: i32, site: String, provider: &'static str, name: &'static str },
    Prompt(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Tab(u64),
    TabClose(u64),
    NewTab,
    Back,
    Forward,
    Omnibox,
    Extensions,
    Inspector,
    Update,
    Brand,
    Star,
    Key,
    Tile(usize),
    Pane(Pane),
}

/// The two web views a tab can show side by side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pane {
    Page,
    DevTools,
}

#[derive(Default)]
struct Layout {
    /// Every tab including ones animating closed, in strip order.
    tabs: Vec<(u64, Rect)>,
    new_tab: Rect,
    back: Rect,
    forward: Rect,
    omnibox: Rect,
    extensions: Rect,
    inspector: Rect,
    update: Rect,
    brand: Rect,
    star: Rect,
    key: Rect,
    tiles: Vec<(Rect, String)>,
    content: Rect,
    page: Rect,
    devtools: Option<Rect>,
}

/// A prompt waiting in the chrome. `browser` ties it to a page: it shows while that page is.
enum Prompt {
    SavePassword { key: u64, browser: i32, url: String, username: String, password: Zeroizing<String> },
    /// Several saved logins fit the form: which one?
    Pick { key: u64, browser: i32, frame: String, logins: Vec<(u64, String)> },
    /// The vault PIN/passphrase before a saved password is used.
    Auth {
        key: u64,
        browser: Option<i32>,
        purpose: Purpose,
        input: Zeroizing<String>,
        checking: Option<mpsc::Receiver<bool>>,
        error: Option<String>,
    },
}

enum Purpose {
    Fill { frame: String, credential: u64 },
    /// A settings-page request (`host::Host::decide`).
    Ticket(u64),
}

impl Prompt {
    fn browser(&self) -> Option<i32> {
        match self {
            Self::SavePassword { browser, .. } | Self::Pick { browser, .. } => Some(*browser),
            Self::Auth { browser, .. } => *browser,
        }
    }
}

/// A login form a page reported: where to fill, and which page it belonged to.
struct Form {
    url: String,
    frame: String,
    page_origin: Option<String>,
}

#[derive(Default)]
struct Omnibox {
    focused: bool,
    text: String,
    /// Freshly focused: the whole text is selected and typing replaces it.
    selected: bool,
}

pub struct Shell {
    tabs: Vec<Tab>,
    active: u64,
    next_id: u64,
    /// The white pill behind the active tab glides between tabs instead of jumping.
    pill_x: Spring,
    pill_w: Spring,
    pill_placed: bool,
    /// Content slides in from the side the newly active tab is on.
    content_shift: Spring,
    content_fade: Spring,
    omnibox_hover: Spring,
    new_tab_hover: Spring,
    extensions_hover: Spring,
    inspector_hover: Spring,
    cursor: Option<(f32, f32)>,
    modifiers: ModifiersState,
    layout: Layout,
    omnibox: Omnibox,
    panel: Panel,
    inspector: Inspector,
    menu: MenuView,
    /// Which pane's context menu is on screen.
    menu_owner: Option<Pane>,
    /// The app menu (under the logo), when open.
    app_menu: Option<MenuView>,
    host: Rc<Host>,
    sheet: SheetView,
    prompts: Vec<Prompt>,
    next_prompt: u64,
    /// Login forms by page (browser id).
    forms: HashMap<i32, Form>,
    /// The omnibox's loading ring: progress glides, then the ring fades out.
    load_progress: Spring,
    load_alpha: Spring,
    load_started: Option<(u64, Instant)>,
    load_finishing: bool,
    star_hover: Spring,
    /// The star pops when a page is bookmarked.
    star_pop: Spring,
    netinfo: Option<Arc<NetInfo>>,
    netinfo_dir: PathBuf,
    /// Redraw from background threads.
    wake: Arc<dyn Fn() + Send + Sync>,
    vpn: Arc<Mutex<VpnStatus>>,
    shield: Arc<shield::Shield>,
    gpu: engine::Gpu,
    notify: Rc<dyn Fn()>,
    scale: f32,
    spin: f32,
    /// Button held on a pane: its release goes there even outside it.
    pressed: Option<(Pane, engine::Button)>,
    last_click: Option<(Instant, f32, f32, i32)>,
    hover_pane: Option<Pane>,
    /// Which pane gets keyboard input.
    focus: Pane,
    last_summary: Option<String>,
    /// A newer build is installed; the bar offers a restart into it.
    update_ready: bool,
    restart_requested: bool,
}

impl Shell {
    pub fn new(
        vpn: Arc<Mutex<VpnStatus>>,
        shield: Arc<shield::Shield>,
        gpu: engine::Gpu,
        notify: Rc<dyn Fn()>,
        netinfo_dir: PathBuf,
        wake: Arc<dyn Fn() + Send + Sync>,
        host: Rc<Host>,
    ) -> Self {
        let mut shell = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            pill_x: Spring::new(0.0, SpringConfig::SETTLE),
            pill_w: Spring::new(0.0, SpringConfig::SETTLE),
            pill_placed: false,
            content_shift: Spring::new(0.0, SpringConfig::GLIDE),
            content_fade: Spring::new(1.0, SpringConfig::GLIDE),
            omnibox_hover: Spring::new(0.0, SpringConfig::SNAP),
            new_tab_hover: Spring::new(0.0, SpringConfig::SNAP),
            extensions_hover: Spring::new(0.0, SpringConfig::SNAP),
            inspector_hover: Spring::new(0.0, SpringConfig::SNAP),
            cursor: None,
            modifiers: ModifiersState::empty(),
            layout: Layout::default(),
            omnibox: Omnibox::default(),
            panel: Panel::new(),
            inspector: Inspector::new(),
            menu: MenuView::new(),
            menu_owner: None,
            app_menu: None,
            host,
            sheet: SheetView::new(),
            prompts: Vec::new(),
            next_prompt: 1,
            forms: HashMap::new(),
            load_progress: Spring::new(0.0, SpringConfig::GLIDE),
            load_alpha: Spring::new(0.0, SpringConfig::GLIDE),
            load_started: None,
            load_finishing: false,
            star_hover: Spring::new(0.0, SpringConfig::SNAP),
            star_pop: Spring::new(1.0, SpringConfig::SETTLE),
            netinfo: None,
            netinfo_dir,
            wake,
            vpn,
            shield,
            gpu,
            notify,
            scale: 1.0,
            spin: 0.0,
            pressed: None,
            last_click: None,
            hover_pane: None,
            focus: Pane::Page,
            last_summary: None,
            update_ready: false,
            restart_requested: false,
        };
        shell.new_tab(None);
        shell
    }

    // ── tabs ───────────────────────────────────────────────────────────────

    pub fn new_tab(&mut self, url: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            page: None,
            devtools: None,
            presence: Spring::new(0.0, SpringConfig::SETTLE),
            hover: Spring::new(0.0, SpringConfig::SNAP),
            closing: false,
            back: Vec::new(),
            forward: Vec::new(),
        });
        self.tabs.last_mut().unwrap().presence.set(1.0);
        self.activate(id);
        match url {
            Some(url) => self.navigate(&url),
            None => self.focus_omnibox(),
        }
    }

    /// A page Chromium created (a popup) becomes a tab of its own.
    fn adopt(&mut self, page: engine::Tab) {
        let id = self.next_id;
        self.next_id += 1;
        let mut presence = Spring::new(0.0, SpringConfig::SETTLE);
        presence.set(1.0);
        self.tabs.push(Tab {
            id,
            page: Some(page),
            devtools: None,
            presence,
            hover: Spring::new(0.0, SpringConfig::SNAP),
            closing: false,
            back: Vec::new(),
            forward: Vec::new(),
        });
        self.activate(id);
        if let Some(page) = self.page() {
            page.set_visible(true);
            page.focus(true);
        }
    }

    /// Back/forward: within the page's own history first, then across site sandboxes.
    fn go(&mut self, step: Step) {
        let active = self.active;
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active) else { return };
        let Some(page) = &tab.page else { return };
        let (can_back, can_forward) = {
            let s = page.state.borrow();
            (s.can_go_back, s.can_go_forward)
        };
        let url = match step {
            Step::Back if can_back => return page.back(),
            Step::Forward if can_forward => return page.forward(),
            Step::Back => tab.back.pop(),
            Step::Forward => tab.forward.pop(),
            Step::New => None,
        };
        if let Some(url) = url {
            self.move_to_sandbox(active, &url, step);
        }
    }

    /// The page in tab `id` is going to `url` in another site's sandbox: a fresh page there
    /// replaces it (Chromium can't move a page between cookie jars).
    fn move_to_sandbox(&mut self, id: u64, url: &str, step: Step) {
        let content = self.layout.content;
        let (w, h) = if content.w > 0.0 { (content.w, content.h) } else { (1200.0, 700.0) };
        let Some(page) = engine::Tab::create(url, self.gpu.clone(), w, h, self.scale, self.notify.clone()) else { return };
        let active = id == self.active;
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        if let Some(old) = tab.page.take() {
            let from = old.state.borrow().url.clone();
            match step {
                Step::New => {
                    tab.back.push(from);
                    tab.forward.clear();
                }
                Step::Back => tab.forward.push(from),
                Step::Forward => tab.back.push(from),
            }
            if let Some(devtools) = tab.devtools.take() {
                devtools.close();
            }
            old.close();
        }
        page.set_visible(active);
        page.focus(active && !self.omnibox.focused);
        tab.page = Some(page);
        if active {
            self.focus = Pane::Page;
        }
    }

    /// Switches to a tab already showing `prefix`, or opens one.
    fn open_internal(&mut self, url: &str) {
        let existing = self.open_tabs().find(|t| t.page.as_ref().is_some_and(|p| p.state.borrow().url.starts_with(url))).map(|t| t.id);
        match existing {
            Some(id) => {
                self.activate(id);
                if let Some(p) = self.page() {
                    p.reload();
                }
            }
            None if self.page().is_none() => self.navigate(url),
            None => self.new_tab(Some(url.to_owned())),
        }
    }

    pub fn close_active(&mut self) {
        self.close(self.active);
    }

    fn close(&mut self, id: u64) {
        let open: Vec<u64> = self.open_tabs().map(|t| t.id).collect();
        let Some(pos) = open.iter().position(|&t| t == id) else { return };
        if open.len() == 1 {
            // Last tab: back to the start page instead of an empty window.
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                if let Some(devtools) = tab.devtools.take() {
                    devtools.close();
                }
                if let Some(page) = tab.page.take() {
                    page.close();
                }
            }
            self.focus_omnibox();
            return;
        }
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.closing = true;
            tab.presence.set(0.0);
            if let Some(devtools) = tab.devtools.take() {
                devtools.close();
            }
            if let Some(page) = &tab.page {
                page.close();
            }
        }
        if id == self.active {
            let next = open.get(pos + 1).or_else(|| open.get(pos.wrapping_sub(1))).copied();
            if let Some(next) = next {
                self.activate(next);
            }
        }
    }

    pub fn cycle(&mut self, step: isize) {
        let open: Vec<u64> = self.open_tabs().map(|t| t.id).collect();
        let Some(pos) = open.iter().position(|&t| t == self.active) else { return };
        let next = (pos as isize + step).rem_euclid(open.len() as isize) as usize;
        self.activate(open[next]);
    }

    pub fn select_index(&mut self, index: usize) {
        let id = self.open_tabs().nth(index).map(|t| t.id);
        if let Some(id) = id {
            self.activate(id);
        }
    }

    pub fn select_last(&mut self) {
        let id = self.open_tabs().last().map(|t| t.id);
        if let Some(id) = id {
            self.activate(id);
        }
    }

    fn activate(&mut self, id: u64) {
        if id == self.active {
            return;
        }
        let order = |id: u64| self.tabs.iter().position(|t| t.id == id);
        let direction = match (order(self.active), order(id)) {
            (Some(from), Some(to)) if to < from => -1.0,
            _ => 1.0,
        };
        if let Some(page) = self.page() {
            page.set_visible(false);
            page.focus(false);
        }
        self.active = id;
        if let Some(page) = self.page() {
            page.set_visible(true);
        }
        self.omnibox.focused = false;
        self.content_shift.snap_to(28.0 * direction);
        self.content_shift.set(0.0);
        self.content_fade.snap_to(0.0);
        self.content_fade.set(1.0);
    }

    fn open_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter(|t| !t.closing)
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active)
    }

    fn page(&self) -> Option<&engine::Tab> {
        self.active_tab()?.page.as_ref()
    }

    fn navigate(&mut self, input: &str) {
        let url = to_url(input, self.host.search().prefix());
        self.omnibox.focused = false;
        if let Some(page) = self.page() {
            page.navigate(&url);
            page.focus(true);
            return;
        }
        let content = self.layout.content;
        let (w, h) = if content.w > 0.0 { (content.w, content.h) } else { (1200.0, 700.0) };
        let page = engine::Tab::create(&url, self.gpu.clone(), w, h, self.scale, self.notify.clone());
        let active = self.active;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active) {
            if let Some(page) = &page {
                page.focus(true);
            }
            tab.page = page;
        }
    }

    /// Loads the first URL in the start tab and opens the rest as new tabs.
    pub fn open_initial<'a>(&mut self, urls: impl Iterator<Item = &'a str>) {
        for (i, url) in urls.enumerate() {
            if i == 0 {
                self.navigate(url);
            } else {
                self.new_tab(Some(url.to_owned()));
            }
        }
    }

    /// Dev builds: raises a prompt (`auth` or `save`) to check how it looks.
    #[cfg(debug_assertions)]
    pub fn dev_prompt(&mut self, kind: &str) {
        let key = self.prompt_key();
        let browser = self.page().and_then(|p| p.browser_id());
        match (kind, browser) {
            ("save", Some(browser)) => self.prompts.push(Prompt::SavePassword {
                key,
                browser,
                url: "https://github.com/login".into(),
                username: "itsArial".into(),
                password: Zeroizing::new("hunter2".into()),
            }),
            _ => self.prompts.push(Prompt::Auth {
                key,
                browser: None,
                purpose: Purpose::Ticket(0),
                input: Zeroizing::new("1234".into()),
                checking: None,
                error: None,
            }),
        }
    }

    pub fn set_update_ready(&mut self) {
        self.update_ready = true;
    }

    /// The user clicked "restart" on the update pill.
    pub fn take_restart_request(&mut self) -> bool {
        std::mem::take(&mut self.restart_requested)
    }

    /// The cursor is over empty bar/tab-strip space, where dragging moves the window.
    pub fn drag_area(&self) -> bool {
        let Some((x, y)) = self.cursor else { return false };
        y < CONTENT_Y - 4.0 && self.hit().is_none() && !self.overlay_at(x, y)
    }

    pub fn open_devtools(&mut self) {
        self.toggle_devtools(false);
    }

    pub fn toggle_panel(&mut self) {
        self.panel.toggle();
    }

    /// Closes every page; call before shutting CEF down.
    pub fn close_all(&mut self) {
        for tab in &mut self.tabs {
            for view in tab.devtools.iter().chain(tab.page.iter()) {
                view.close();
            }
        }
    }

    pub fn all_closed(&self) -> bool {
        self.tabs.iter().flat_map(|t| t.page.iter().chain(t.devtools.iter())).all(|p| p.state.borrow().closed)
    }

    // ── omnibox ────────────────────────────────────────────────────────────

    fn focus_omnibox(&mut self) {
        let url = self.page().map(|p| p.state.borrow().url.clone()).unwrap_or_default();
        self.omnibox = Omnibox { focused: true, text: url, selected: true };
        if let Some(page) = self.page() {
            page.focus(false);
        }
    }

    fn omnibox_key(&mut self, event: &KeyEvent) {
        let ctrl = self.modifiers.control_key();
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                let text = self.omnibox.text.trim().to_owned();
                if !text.is_empty() {
                    self.navigate(&text);
                }
            }
            Key::Named(NamedKey::Escape) => {
                self.omnibox.focused = false;
                if let Some(page) = self.page() {
                    page.focus(true);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if self.omnibox.selected || ctrl {
                    self.omnibox.text.clear();
                } else {
                    self.omnibox.text.pop();
                }
                self.omnibox.selected = false;
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => self.omnibox.selected = true,
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                let pasted = paste();
                if self.omnibox.selected {
                    self.omnibox.text.clear();
                }
                self.omnibox.text.push_str(pasted.trim());
                self.omnibox.selected = false;
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => copy(&self.omnibox.text),
            _ if !ctrl => {
                if let Some(text) = &event.text {
                    let text: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !text.is_empty() {
                        if self.omnibox.selected {
                            self.omnibox.text.clear();
                        }
                        self.omnibox.text.push_str(&text);
                        self.omnibox.selected = false;
                    }
                }
            }
            _ => {}
        }
    }

    // ── input ──────────────────────────────────────────────────────────────

    pub fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    pub fn key(&mut self, event: &KeyEvent) {
        let pressed = event.state == ElementState::Pressed;
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();

        if pressed && self.sheet_key(event) {
            return;
        }
        if pressed {
            let handled = match &event.logical_key {
                Key::Named(NamedKey::Escape) if self.app_menu.is_some() => {
                    self.app_menu = None;
                    true
                }
                Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("o") => {
                    self.open_internal(BOOKMARKS_URL);
                    true
                }
                Key::Character(c) if ctrl && c == "," => {
                    self.open_internal(SETTINGS_URL);
                    true
                }
                Key::Character(c) if ctrl && !shift && c.eq_ignore_ascii_case("d") => {
                    self.toggle_bookmark();
                    true
                }
                Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("i") => {
                    self.toggle_devtools(false);
                    true
                }
                Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("d") => {
                    self.toggle_inspector();
                    true
                }
                Key::Character(c) if ctrl => match c.to_ascii_lowercase().as_str() {
                    "t" => {
                        self.new_tab(None);
                        true
                    }
                    "w" => {
                        self.close_active();
                        true
                    }
                    "l" => {
                        self.focus_omnibox();
                        true
                    }
                    "r" => {
                        if let Some(p) = self.page() {
                            p.reload();
                        }
                        true
                    }
                    "9" => {
                        self.select_last();
                        true
                    }
                    d if d.len() == 1 && ("1"..="8").contains(&d) => {
                        self.select_index(d.parse::<usize>().unwrap() - 1);
                        true
                    }
                    _ => false,
                },
                Key::Named(NamedKey::Tab) if ctrl => {
                    self.cycle(if shift { -1 } else { 1 });
                    true
                }
                Key::Named(NamedKey::F12) => {
                    self.toggle_devtools(false);
                    true
                }
                Key::Named(NamedKey::F5) => {
                    if let Some(p) = self.page() {
                        p.reload();
                    }
                    true
                }
                Key::Named(NamedKey::ArrowLeft) if alt => {
                    self.go(Step::Back);
                    true
                }
                Key::Named(NamedKey::ArrowRight) if alt => {
                    self.go(Step::Forward);
                    true
                }
                Key::Named(NamedKey::Escape) if self.menu_owner.is_some() => {
                    self.finish_menu(None);
                    true
                }
                Key::Named(NamedKey::Escape) if self.inspector.is_open() => {
                    self.inspector.close();
                    true
                }
                Key::Named(NamedKey::Escape) if self.panel.is_open() => {
                    self.panel.close();
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }

        if self.omnibox.focused {
            if pressed {
                self.omnibox_key(event);
            }
            return;
        }
        if let Some(view) = self.pane_view(self.focus).or_else(|| self.page()) {
            for key in keys::translate(event, self.modifiers) {
                view.key(key);
            }
        }
    }

    // ── panes (page + docked DevTools) ─────────────────────────────────────

    fn pane_view(&self, pane: Pane) -> Option<&engine::Tab> {
        let tab = self.active_tab()?;
        match pane {
            Pane::Page => tab.page.as_ref(),
            Pane::DevTools => tab.devtools.as_ref(),
        }
    }

    fn pane_rect(&self, pane: Pane) -> Rect {
        match pane {
            Pane::Page => self.layout.page,
            Pane::DevTools => self.layout.devtools.unwrap_or_default(),
        }
    }

    /// The pane under (x, y), unless a panel or menu covers it.
    fn pane_at(&self, x: f32, y: f32) -> Option<Pane> {
        if self.overlay_at(x, y) {
            return None;
        }
        if self.layout.devtools.is_some_and(|r| r.contains(x, y)) {
            return Some(Pane::DevTools);
        }
        self.layout.page.contains(x, y).then_some(Pane::Page)
    }

    fn overlay_at(&self, x: f32, y: f32) -> bool {
        self.panel.hit(x, y).is_some()
            || self.inspector.hit(x, y).is_some()
            || self.menu_owner.is_some()
            || self.app_menu.is_some()
            || (self.sheet.visible() && self.sheet.hit(x, y).is_some())
    }

    /// Opens or closes DevTools docked next to the page; with `keep_open` (context-menu "Inspect")
    /// an open DevTools stays open.
    fn toggle_devtools(&mut self, keep_open: bool) {
        let (gpu, notify, scale, wake) = (self.gpu.clone(), self.notify.clone(), self.scale, self.wake.clone());
        let c = self.layout.content;
        let active = self.active;
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == active) else { return };
        let Some(page) = &tab.page else { return };
        match &tab.devtools {
            Some(devtools) if !keep_open => {
                devtools.close();
                tab.devtools = None;
                self.focus = Pane::Page;
            }
            Some(_) => {}
            None => {
                tab.devtools = page.open_devtools(gpu, c.w * 0.42, c.h, scale, notify, wake);
                self.focus = Pane::DevTools;
            }
        }
    }

    pub fn toggle_inspector(&mut self) {
        self.panel.close();
        self.inspector.toggle();
        if self.inspector.is_open() && self.netinfo.is_none() {
            // The offline IP database is only fetched once the inspector is first used.
            let wake = self.wake.clone();
            self.netinfo = Some(NetInfo::new(self.netinfo_dir.clone(), move || wake()));
        }
    }

    // ── context menu ───────────────────────────────────────────────────────

    /// Picks up a menu a pane just asked for.
    fn poll_menu(&mut self) {
        if self.menu_owner.is_some() {
            return;
        }
        for pane in [Pane::Page, Pane::DevTools] {
            if self.pane_view(pane).is_some_and(|v| v.state.borrow().context_menu.is_some()) {
                self.menu_owner = Some(pane);
                self.menu.reset();
                return;
            }
        }
    }

    /// `index` into the menu's items, or `None` to dismiss.
    fn finish_menu(&mut self, index: Option<usize>) {
        let Some(owner) = self.menu_owner.take() else { return };
        let Some(view) = self.pane_view(owner) else { return };
        let picked = view.state.borrow().context_menu.as_ref().and_then(|m| {
            let item = m.items.get(index?)?;
            Some((item.id, m.link.clone()))
        });
        let Some((id, link)) = picked else {
            view.finish_context_menu(None);
            return;
        };
        match id {
            engine::commands::OPEN_LINK_IN_TAB => {
                view.finish_context_menu(None);
                if let Some(link) = link {
                    self.new_tab(Some(link));
                }
            }
            engine::commands::COPY_LINK => {
                view.finish_context_menu(None);
                copy(link.as_deref().unwrap_or_default());
            }
            engine::commands::INSPECT => {
                view.finish_context_menu(None);
                if owner == Pane::Page {
                    self.toggle_devtools(true);
                }
            }
            id => view.finish_context_menu(Some(id)),
        }
    }

    // ── app menu, bookmarks ────────────────────────────────────────────────

    fn app_menu_rows(&self) -> Vec<(AppAction, Row<'static>)> {
        let bookmarked = self.page().is_some_and(|p| self.host.profile.borrow().is_bookmarked(&p.state.borrow().url));
        let mut rows = vec![
            (AppAction::NewTab, Row::item("new tab", "ctrl+t")),
            (AppAction::Bookmark, Row::item(if bookmarked { "remove bookmark" } else { "bookmark this page" }, "ctrl+d")),
            (AppAction::Bookmarks, Row::item("bookmarks", "ctrl+shift+o")),
            (AppAction::Settings, Row::item("settings", "ctrl+,")),
            (AppAction::None, Row::separator()),
            (AppAction::DevTools, Row::item("developer tools", "f12")),
            (AppAction::Inspector, Row::item("site inspector", "ctrl+shift+d")),
        ];
        if self.update_ready {
            rows.push((AppAction::None, Row::separator()));
            rows.push((AppAction::Restart, Row::item("update · restart", "")));
        }
        rows
    }

    fn app_menu_pick(&mut self, index: usize) {
        let Some(action) = self.app_menu_rows().get(index).map(|(a, _)| *a) else { return };
        match action {
            AppAction::NewTab => self.new_tab(None),
            AppAction::Bookmark => self.toggle_bookmark(),
            AppAction::Bookmarks => self.open_internal(BOOKMARKS_URL),
            AppAction::Settings => self.open_internal(SETTINGS_URL),
            AppAction::DevTools => self.toggle_devtools(false),
            AppAction::Inspector => self.toggle_inspector(),
            AppAction::Restart => self.restart_requested = true,
            AppAction::None => {}
        }
    }

    fn toggle_bookmark(&mut self) {
        let Some((url, title)) = self.page().map(|p| {
            let s = p.state.borrow();
            (s.url.clone(), s.title.clone())
        }) else {
            return;
        };
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return;
        }
        if self.host.profile.borrow_mut().toggle_bookmark(&url, &title) {
            self.star_pop.snap_to(1.6);
            self.star_pop.set(1.0);
        }
    }

    // ── prompts (sign-in sharing, passwords) ───────────────────────────────

    fn page_by_browser(&self, browser: i32) -> Option<&engine::Tab> {
        self.tabs.iter().filter_map(|t| t.page.as_ref()).find(|p| p.browser_id() == Some(browser))
    }

    fn prompt_key(&mut self) -> u64 {
        self.next_prompt += 1;
        self.next_prompt
    }

    /// What the sheet shows now: the page's sign-in question first, then its queued prompts.
    fn current(&self) -> Option<Current> {
        let page = self.page();
        if let Some((site, provider, name)) = page.and_then(|p| p.identity_prompt()) {
            return Some(Current::Identity { browser: page?.browser_id()?, site, provider, name });
        }
        let browser = page.and_then(|p| p.browser_id());
        self.prompts.iter().position(|p| p.browser().is_none() || p.browser() == browser).map(Current::Prompt)
    }

    fn sheet_spec(&self, current: &Current) -> (u64, Spec) {
        let host_of = |url: &str| display_url(url).split('/').next().unwrap_or_default().to_owned();
        match current {
            Current::Identity { browser, site, name, .. } => (
                (1 << 40) | *browser as u64,
                Spec {
                    title: format!("{site} wants your {name} sign-in"),
                    body: format!(
                        "until you allow it, {name} sees you signed out here: {site} can't tell you have an account, and {name} doesn't learn you came from {site}."
                    ),
                    rows: Vec::new(),
                    input: None,
                    buttons: vec!["stay signed out", "allow once", "always allow"],
                },
            ),
            Current::Prompt(i) => match &self.prompts[*i] {
                Prompt::SavePassword { key, url, username, .. } => (
                    *key,
                    Spec {
                        title: "save this password?".into(),
                        body: format!(
                            "{} on {}. it's kept in your encrypted vault on this machine and never synced.",
                            if username.is_empty() { "a login" } else { username },
                            host_of(url)
                        ),
                        rows: Vec::new(),
                        input: None,
                        buttons: vec!["never here", "not now", "save"],
                    },
                ),
                Prompt::Pick { key, logins, .. } => (
                    *key,
                    Spec {
                        title: "choose a login".into(),
                        body: "saved for this site.".into(),
                        rows: logins.iter().map(|(_, user)| (if user.is_empty() { "(no username)".into() } else { user.clone() }, "fill".into())).collect(),
                        input: None,
                        buttons: vec!["cancel"],
                    },
                ),
                Prompt::Auth { key, purpose, input, checking, error, .. } => {
                    let passphrase = matches!(
                        self.host.vault.state(),
                        vault::State::Locked { protection: vault::Protection::Passphrase } | vault::State::New { protection: vault::Protection::Passphrase }
                    );
                    (
                        *key,
                        Spec {
                            title: match purpose {
                                Purpose::Fill { .. } => "unlock to fill this login".into(),
                                Purpose::Ticket(_) => "confirm it's you".into(),
                            },
                            body: format!(
                                "type the {} you unlock the browser with. you can turn this off in settings → passwords.",
                                if passphrase { "passphrase" } else { "pin" }
                            ),
                            rows: Vec::new(),
                            input: Some(Input {
                                hint: if passphrase { "passphrase" } else { "pin" },
                                len: input.chars().count(),
                                busy: checking.is_some(),
                                error: error.clone(),
                            }),
                            buttons: vec!["cancel", "unlock"],
                        },
                    )
                }
            },
        }
    }

    fn sheet_click(&mut self, hit: SheetHit) {
        let Some(current) = self.current() else { return };
        match (current, hit) {
            (Current::Identity { site, provider, .. }, SheetHit::Button(b)) => {
                if b == 2 {
                    self.host.set_identity(&site, provider, Some(profile::Decision::Allow));
                }
                if let Some(page) = self.page() {
                    page.answer_identity(b > 0);
                }
            }
            (Current::Prompt(i), hit) => match (&self.prompts[i], hit) {
                (Prompt::SavePassword { .. }, SheetHit::Button(b)) => {
                    let Prompt::SavePassword { url, username, password, .. } = self.prompts.remove(i) else { return };
                    match b {
                        0 => self.host.profile.borrow_mut().never_save_on(&url),
                        2 => self.host.profile.borrow_mut().save_login(&url, &username, &password),
                        _ => {}
                    }
                }
                (Prompt::Pick { .. }, SheetHit::Row(r)) => {
                    let Prompt::Pick { browser, frame, logins, .. } = self.prompts.remove(i) else { return };
                    if let Some(&(credential, _)) = logins.get(r) {
                        self.request_fill(browser, frame, credential);
                    }
                }
                (Prompt::Pick { .. }, SheetHit::Button(_)) => {
                    self.prompts.remove(i);
                }
                (Prompt::Auth { .. }, SheetHit::Button(0)) => self.cancel_auth(i),
                (Prompt::Auth { .. }, SheetHit::Button(_)) => self.submit_auth(i),
                _ => {}
            },
            _ => {}
        }
    }

    /// Keys for the sheet on screen. True if it took the key.
    fn sheet_key(&mut self, event: &KeyEvent) -> bool {
        let Some(current) = self.current() else { return false };
        let escape = matches!(event.logical_key, Key::Named(NamedKey::Escape));
        match current {
            Current::Identity { .. } if escape => {
                self.sheet_click(SheetHit::Button(0));
                true
            }
            Current::Prompt(i) => match &mut self.prompts[i] {
                Prompt::Auth { input, checking, error, .. } => {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.cancel_auth(i),
                        Key::Named(NamedKey::Enter) => self.submit_auth(i),
                        _ if checking.is_some() => {}
                        Key::Named(NamedKey::Backspace) => {
                            input.pop();
                        }
                        _ if !self.modifiers.control_key() => {
                            if let Some(text) = &event.text {
                                for c in text.chars().filter(|c| !c.is_control()) {
                                    if input.len() < 128 {
                                        input.push(c);
                                    }
                                }
                                *error = None;
                            }
                        }
                        _ => {}
                    }
                    true
                }
                _ if escape => {
                    self.prompts.remove(i);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// The key in the address bar: fill a saved login into this page's form.
    fn use_key(&mut self) {
        let Some(browser) = self.page().and_then(|p| p.browser_id()) else { return };
        let Some(form) = self.forms.get(&browser) else { return };
        let frame = form.frame.clone();
        let logins: Vec<(u64, String)> =
            self.host.profile.borrow().logins_for(&form.url).iter().map(|c| (c.id, c.username.clone())).collect();
        match logins.as_slice() {
            [] => {}
            [(credential, _)] => self.request_fill(browser, frame, *credential),
            _ => {
                let key = self.prompt_key();
                self.prompts.retain(|p| !matches!(p, Prompt::Pick { browser: b, .. } if *b == browser));
                self.prompts.push(Prompt::Pick { key, browser, frame, logins });
            }
        }
    }

    fn request_fill(&mut self, browser: i32, frame: String, credential: u64) {
        if self.host.prefs().confirm_password_use {
            let key = self.prompt_key();
            self.prompts.push(Prompt::Auth {
                key,
                browser: Some(browser),
                purpose: Purpose::Fill { frame, credential },
                input: Zeroizing::new(String::new()),
                checking: None,
                error: None,
            });
        } else {
            self.fill(browser, &frame, credential);
        }
    }

    fn fill(&self, browser: i32, frame: &str, credential: u64) {
        let login = {
            let mut profile = self.host.profile.borrow_mut();
            let login = profile.passwords().iter().find(|c| c.id == credential).map(|c| (c.username.clone(), Zeroizing::new(c.password.clone())));
            profile.mark_used(credential);
            login
        };
        if let (Some((username, password)), Some(page)) = (login, self.page_by_browser(browser)) {
            page.fill_login(frame, &username, &password);
        }
    }

    fn submit_auth(&mut self, index: usize) {
        let Some(Prompt::Auth { input, checking, error, .. }) = self.prompts.get_mut(index) else { return };
        if checking.is_some() || input.is_empty() {
            return;
        }
        let pin = input.clone();
        let (vault, wake) = (self.host.vault.clone(), self.wake.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(vault.verify(&pin).is_ok());
            wake();
        });
        *checking = Some(rx);
        *error = None;
    }

    fn cancel_auth(&mut self, index: usize) {
        if let Prompt::Auth { purpose: Purpose::Ticket(ticket), .. } = self.prompts.remove(index) {
            self.host.decide(ticket, false);
        }
    }

    /// Finished PIN checks.
    fn poll_auth(&mut self) {
        let mut granted = None;
        let mut wrong = false;
        for (i, prompt) in self.prompts.iter_mut().enumerate() {
            let Prompt::Auth { input, checking: checking @ Some(_), error, .. } = prompt else { continue };
            match checking.as_ref().unwrap().try_recv() {
                Ok(true) => granted = Some(i),
                Ok(false) | Err(mpsc::TryRecvError::Disconnected) => {
                    *checking = None;
                    input.clear();
                    *error = Some("that's not it. try again".into());
                    wrong = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if wrong {
            self.sheet.shake();
        }
        let Some(i) = granted else { return };
        if let Prompt::Auth { browser, purpose, .. } = self.prompts.remove(i) {
            match purpose {
                Purpose::Fill { frame, credential } => {
                    if let Some(browser) = browser {
                        self.fill(browser, &frame, credential);
                    }
                }
                Purpose::Ticket(ticket) => self.host.decide(ticket, true),
            }
        }
    }

    fn handle_host_events(&mut self) {
        for event in self.host.take_events() {
            match event {
                Event::OpenUrl(url) => self.new_tab(Some(url)),
                Event::Authorize(ticket) => {
                    let key = self.prompt_key();
                    self.prompts.push(Prompt::Auth {
                        key,
                        browser: None,
                        purpose: Purpose::Ticket(ticket),
                        input: Zeroizing::new(String::new()),
                        checking: None,
                        error: None,
                    });
                }
                Event::LoginSubmitted { browser, url, username, password } => {
                    let key = self.prompt_key();
                    self.prompts.retain(|p| !matches!(p, Prompt::SavePassword { browser: b, .. } if *b == browser));
                    self.prompts.push(Prompt::SavePassword { key, browser, url, username, password });
                }
                Event::LoginForm { browser, url, frame } => {
                    let page_origin = self.page_by_browser(browser).and_then(|p| profile::origin_of(&p.state.borrow().url));
                    self.forms.insert(browser, Form { url, frame, page_origin });
                }
            }
        }
    }

    /// A saved login fits the form on this page.
    fn key_available(&self) -> bool {
        let Some(page) = self.page() else { return false };
        let Some(form) = page.browser_id().and_then(|b| self.forms.get(&b)) else { return false };
        form.page_origin == profile::origin_of(&page.state.borrow().url) && !self.host.profile.borrow().logins_for(&form.url).is_empty()
    }

    /// Glides the omnibox's loading ring toward the page's progress; fades it when done.
    fn update_loading(&mut self, dt: f32) -> bool {
        let state = self.page().map(|p| {
            let s = p.state.borrow();
            (s.loading, s.progress)
        });
        match state {
            Some((true, progress)) => {
                if !matches!(self.load_started, Some((id, _)) if id == self.active) {
                    self.load_started = Some((self.active, Instant::now()));
                    self.load_progress.snap_to(0.0);
                    self.load_alpha.set(1.0);
                    self.load_finishing = false;
                }
                // Chromium's progress stalls on slow servers: creep forward so the ring never freezes.
                let t = self.load_started.map_or(0.0, |(_, at)| at.elapsed().as_secs_f32());
                let creep = 0.8 * (1.0 - (-t / 3.0).exp());
                let target = progress.max(creep).min(0.97).max(self.load_progress.target);
                self.load_progress.set(target);
            }
            _ => match self.load_started.take() {
                Some((id, _)) if id == self.active => {
                    self.load_progress.set(1.0);
                    self.load_finishing = true;
                }
                Some(_) => {
                    self.load_alpha.snap_to(0.0);
                    self.load_finishing = false;
                }
                None => {}
            },
        }
        if self.load_finishing && self.load_progress.value > 0.96 {
            self.load_alpha.set(0.0);
            self.load_finishing = false;
        }
        self.load_progress.step(dt) | self.load_alpha.step(dt)
    }

    // ── pointer ────────────────────────────────────────────────────────────

    pub fn cursor_moved(&mut self, x: f32, y: f32) {
        self.cursor = Some((x, y));
        if self.menu_owner.is_some() {
            self.menu.set_hover(x, y);
            return;
        }
        if let Some(view) = &mut self.app_menu {
            view.set_hover(x, y);
            return;
        }
        self.sheet.set_hover(Some((x, y)));
        let mods = keys::flags(self.modifiers) | self.button_flags();
        // While a button is held, the pane that got the press keeps the pointer.
        let target = self.pressed.map(|(p, _)| p).or_else(|| self.pane_at(x, y));
        if let Some(previous) = self.hover_pane.filter(|p| Some(*p) != target)
            && let Some(view) = self.pane_view(previous) {
                let r = self.pane_rect(previous);
                view.mouse_move(x - r.x, y - r.y, mods, true);
            }
        if let Some(pane) = target
            && let Some(view) = self.pane_view(pane) {
                let r = self.pane_rect(pane);
                view.mouse_move(x - r.x, y - r.y, mods, false);
            }
        self.hover_pane = target;
    }

    pub fn cursor_left(&mut self) {
        if let (Some(pane), Some((x, y))) = (self.hover_pane, self.cursor)
            && let Some(view) = self.pane_view(pane) {
                let r = self.pane_rect(pane);
                view.mouse_move(x - r.x, y - r.y, 0, true);
            }
        self.cursor = None;
        self.hover_pane = None;
    }

    fn button_flags(&self) -> u32 {
        match self.pressed.map(|(_, b)| b) {
            Some(engine::Button::Left) => engine::modifiers::LEFT_BUTTON,
            Some(engine::Button::Middle) => engine::modifiers::MIDDLE_BUTTON,
            Some(engine::Button::Right) => engine::modifiers::RIGHT_BUTTON,
            None => 0,
        }
    }

    pub fn mouse_button(&mut self, button: engine::Button, pressed: bool) {
        let Some((x, y)) = self.cursor else { return };
        let mods = keys::flags(self.modifiers);

        if !pressed {
            if let Some((pane, held)) = self.pressed.take()
                && let Some(view) = self.pane_view(pane) {
                    let r = self.pane_rect(pane);
                    let clicks = self.last_click.map_or(1, |(_, _, _, n)| n);
                    view.mouse_button(x - r.x, y - r.y, held, true, clicks, mods);
                }
            return;
        }

        if self.menu_owner.is_some() {
            match self.menu.hit(x, y) {
                Some(MenuHit::Item(i)) => self.finish_menu(Some(i)),
                Some(MenuHit::Inside) => {}
                None => self.finish_menu(None),
            }
            return;
        }

        if let Some(view) = &self.app_menu {
            match view.hit(x, y) {
                Some(MenuHit::Item(i)) => {
                    self.app_menu = None;
                    if button == engine::Button::Left {
                        self.app_menu_pick(i);
                    }
                }
                Some(MenuHit::Inside) => {}
                None => self.app_menu = None,
            }
            return;
        }

        if self.sheet.visible()
            && let Some(hit) = self.sheet.hit(x, y)
        {
            if button == engine::Button::Left {
                self.sheet_click(hit);
            }
            return;
        }

        if self.inspector.is_open() {
            match self.inspector.hit(x, y) {
                Some(hit) => {
                    if button == engine::Button::Left {
                        self.inspector_click(hit);
                    }
                    return;
                }
                None if self.hit() != Some(Hit::Inspector) => self.inspector.close(),
                None => {}
            }
        }

        if self.panel.is_open() {
            match self.panel.hit(x, y) {
                Some(hit) => {
                    if button == engine::Button::Left {
                        self.panel_click(hit);
                    }
                    return;
                }
                None if self.hit() != Some(Hit::Extensions) => {
                    self.panel.close();
                    return;
                }
                None => {}
            }
        }

        match (self.hit(), button) {
            (Some(Hit::Pane(pane)), _) => {
                self.omnibox.focused = false;
                let now = Instant::now();
                let clicks = match self.last_click {
                    Some((t, px, py, n)) if now - t < std::time::Duration::from_millis(450) && (px - x).abs() < 5.0 && (py - y).abs() < 5.0 => (n % 3) + 1,
                    _ => 1,
                };
                self.last_click = Some((now, x, y, clicks));
                self.focus = pane;
                let other = if pane == Pane::Page { Pane::DevTools } else { Pane::Page };
                if let Some(view) = self.pane_view(other) {
                    view.focus(false);
                }
                if let Some(view) = self.pane_view(pane) {
                    let r = self.pane_rect(pane);
                    view.focus(true);
                    view.mouse_button(x - r.x, y - r.y, button, false, clicks, mods);
                    self.pressed = Some((pane, button));
                }
            }
            (Some(Hit::Tab(id)), engine::Button::Left) => self.activate(id),
            (Some(Hit::Tab(id) | Hit::TabClose(id)), engine::Button::Middle) => self.close(id),
            (Some(Hit::TabClose(id)), engine::Button::Left) => self.close(id),
            (Some(Hit::NewTab), engine::Button::Left) => self.new_tab(None),
            (Some(Hit::Back), engine::Button::Left) => self.go(Step::Back),
            (Some(Hit::Forward), engine::Button::Left) => self.go(Step::Forward),
            (Some(Hit::Omnibox), engine::Button::Left) => {
                if !self.omnibox.focused {
                    self.focus_omnibox();
                }
            }
            (Some(Hit::Extensions), engine::Button::Left) => {
                self.inspector.close();
                self.panel.toggle();
            }
            (Some(Hit::Inspector), engine::Button::Left) => self.toggle_inspector(),
            (Some(Hit::Update), engine::Button::Left) => self.restart_requested = true,
            (Some(Hit::Brand), engine::Button::Left) => {
                self.panel.close();
                self.inspector.close();
                let mut view = MenuView::new();
                view.reset();
                self.app_menu = Some(view);
            }
            (Some(Hit::Star), engine::Button::Left) => self.toggle_bookmark(),
            (Some(Hit::Key), engine::Button::Left) => self.use_key(),
            (Some(Hit::Tile(i)), b) if b != engine::Button::Right => {
                if let Some((_, url)) = self.layout.tiles.get(i).cloned() {
                    if b == engine::Button::Middle {
                        self.new_tab(Some(url));
                    } else {
                        self.navigate(&url);
                    }
                }
            }
            _ => {
                self.omnibox.focused = false;
            }
        }
    }

    fn inspector_click(&mut self, hit: InspectorHit) {
        match hit {
            InspectorHit::Tab(tab) => self.inspector.select(tab),
            InspectorHit::DevTools => {
                self.inspector.close();
                self.toggle_devtools(false);
            }
            InspectorHit::Row(i) => {
                let data = self.inspector_data();
                if let Some(key) = self.inspector.row_key(&data, i) {
                    self.inspector.toggle_row(key);
                }
            }
            InspectorHit::Inside => {}
        }
    }

    fn panel_click(&mut self, hit: PanelHit) {
        let url = self.page().map(|p| p.state.borrow().url.clone()).unwrap_or_default();
        match hit {
            PanelHit::SiteToggle => {
                let excepted = self.shield.is_excepted(&url);
                self.shield.set_excepted(&url, !excepted);
                if let Some(p) = self.page() {
                    p.reload();
                }
            }
            PanelHit::PauseToggle => {
                self.shield.set_paused(!self.shield.is_paused());
                if let Some(p) = self.page() {
                    p.reload();
                }
            }
            PanelHit::Update => self.shield.update_now(),
            PanelHit::Event(i) => {
                let event = self.page().and_then(|p| p.net.lock().unwrap().events.iter().rev().nth(i).cloned());
                if let Some(event) = event {
                    self.panel.toggle_event(&event);
                }
            }
            PanelHit::Inside => {}
        }
    }

    /// `dx`/`dy` in logical pixels, positive = content moves right/down.
    pub fn wheel(&mut self, dx: f32, dy: f32, precise: bool) {
        let Some((x, y)) = self.cursor else { return };
        if self.menu_owner.is_some() || self.app_menu.is_some() {
            return;
        }
        if self.inspector.over_list(x, y) {
            self.inspector.scroll_by(dy);
            return;
        }
        if self.panel.over_log(x, y) {
            self.panel.scroll_by(dy);
            return;
        }
        let Some(pane) = self.pane_at(x, y) else { return };
        let mut mods = keys::flags(self.modifiers);
        if precise {
            mods |= engine::modifiers::PRECISION_SCROLL;
        }
        if let Some(view) = self.pane_view(pane) {
            let r = self.pane_rect(pane);
            view.wheel(x - r.x, y - r.y, dx, dy, mods);
        }
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        if self.menu_owner.is_some() || self.app_menu.is_some() {
            return CursorIcon::Default;
        }
        match self.hit() {
            Some(Hit::Pane(pane)) => match self.pane_view(pane).map(|p| p.state.borrow().cursor).unwrap_or_default() {
                engine::Cursor::Pointer => CursorIcon::Pointer,
                engine::Cursor::Text => CursorIcon::Text,
                engine::Cursor::Wait => CursorIcon::Wait,
                engine::Cursor::Progress => CursorIcon::Progress,
                engine::Cursor::Crosshair => CursorIcon::Crosshair,
                engine::Cursor::Move => CursorIcon::Move,
                engine::Cursor::NotAllowed => CursorIcon::NotAllowed,
                engine::Cursor::Grab => CursorIcon::Grab,
                engine::Cursor::ResizeEw => CursorIcon::EwResize,
                engine::Cursor::ResizeNs => CursorIcon::NsResize,
                engine::Cursor::ResizeNesw => CursorIcon::NeswResize,
                engine::Cursor::ResizeNwse => CursorIcon::NwseResize,
                engine::Cursor::Help => CursorIcon::Help,
                engine::Cursor::Default => CursorIcon::Default,
            },
            Some(Hit::Omnibox) => CursorIcon::Text,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        }
    }

    fn hit(&self) -> Option<Hit> {
        let (x, y) = self.cursor?;
        if self.panel.hit(x, y).is_some()
            || self.inspector.hit(x, y).is_some()
            || (self.sheet.visible() && self.sheet.hit(x, y).is_some())
        {
            return None;
        }
        for &(id, rect) in &self.layout.tabs {
            if self.tabs.iter().any(|t| t.id == id && t.closing) {
                continue;
            }
            if rect.contains(x, y) {
                let close = Rect::new(rect.right() - 28.0, rect.y, 28.0, rect.h);
                return Some(if close.contains(x, y) { Hit::TabClose(id) } else { Hit::Tab(id) });
            }
        }
        let l = &self.layout;
        if let Some(r) = l.devtools
            && r.contains(x, y) {
                return Some(Hit::Pane(Pane::DevTools));
            }
        if let Some(i) = l.tiles.iter().position(|(r, _)| r.contains(x, y)) {
            return Some(Hit::Tile(i));
        }
        [
            (l.new_tab, Hit::NewTab),
            (l.back, Hit::Back),
            (l.forward, Hit::Forward),
            (l.brand, Hit::Brand),
            (l.star, Hit::Star),
            (l.key, Hit::Key),
            (l.omnibox, Hit::Omnibox),
            (l.extensions, Hit::Extensions),
            (l.inspector, Hit::Inspector),
            (l.update, Hit::Update),
            (l.page, Hit::Pane(Pane::Page)),
        ]
        .into_iter()
        .find(|(r, _)| r.contains(x, y))
        .map(|(_, h)| h)
    }

    // ── frame ──────────────────────────────────────────────────────────────

    /// Advances animations by `dt`, lays out and records the frame into `scene`.
    /// Returns `true` while anything is still moving.
    pub fn frame(&mut self, dt: f32, width: f32, height: f32, scale: f32, scene: &mut Scene) -> bool {
        self.scale = scale;
        let mut moving = false;
        for tab in &mut self.tabs {
            moving |= tab.presence.step(dt);
            moving |= tab.hover.step(dt);
        }
        self.tabs.retain(|t| !(t.closing && t.presence.is_settled()));
        for spring in [
            &mut self.pill_x,
            &mut self.pill_w,
            &mut self.content_shift,
            &mut self.content_fade,
            &mut self.omnibox_hover,
            &mut self.new_tab_hover,
            &mut self.extensions_hover,
            &mut self.inspector_hover,
            &mut self.star_hover,
            &mut self.star_pop,
        ] {
            moving |= spring.step(dt);
        }
        moving |= self.panel.step(dt);
        moving |= self.inspector.step(dt);
        moving |= self.menu.step(dt);
        if let Some(view) = &mut self.app_menu {
            moving |= view.step(dt);
        }

        self.handle_host_events();
        self.poll_auth();
        // Popups Chromium created become tabs.
        let popups: Vec<engine::Tab> = self
            .tabs
            .iter()
            .filter_map(|t| t.page.as_ref())
            .flat_map(|p| std::mem::take(&mut p.state.borrow_mut().popups))
            .collect();
        for popup in popups {
            self.adopt(popup);
        }
        // Pages heading to another site move to that site's sandbox.
        let swaps: Vec<(u64, String)> = self
            .tabs
            .iter()
            .filter_map(|t| Some((t.id, t.page.as_ref()?.state.borrow_mut().swap.take()?)))
            .collect();
        for (id, url) in swaps {
            self.move_to_sandbox(id, &url, Step::New);
        }
        // Forms of closed pages.
        let live: Vec<i32> = self.tabs.iter().filter_map(|t| t.page.as_ref()?.browser_id()).collect();
        self.forms.retain(|b, _| live.contains(b));

        // Pages that asked for new windows get tabs.
        let requests: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| t.page.as_ref())
            .flat_map(|p| std::mem::take(&mut p.state.borrow_mut().open_requests))
            .collect();
        for url in requests {
            self.new_tab(Some(url));
        }

        self.layout(width, height);
        self.update_targets();

        for pane in [Pane::Page, Pane::DevTools] {
            let r = self.pane_rect(pane);
            if let Some(view) = self.pane_view(pane) {
                view.resize(r.w, r.h, scale);
                view.begin_frame();
                // Keep the frame clock running while a page is shown.
                moving = true;
            }
        }
        self.poll_menu();

        moving |= self.update_loading(dt);
        let vpn = self.vpn.lock().unwrap().clone();
        let loading = self.page().is_some_and(|p| p.state.borrow().loading);
        if vpn == VpnStatus::Connecting || loading {
            self.spin = (self.spin + dt * 1.4).fract();
            moving = true;
        }

        self.draw_bar(scene, width, &vpn);
        self.draw_tabs(scene);
        self.draw_content(scene);
        if self.panel.open.value > 0.01 {
            let data = self.panel_data();
            let hover = self.cursor.and_then(|(x, y)| self.panel.hit(x, y));
            self.panel.set_hover(hover);
            self.panel.draw(scene, width, height, BAR_Y + BAR_H + 8.0, &data);
        }
        if self.inspector.open.value > 0.01 {
            let data = self.inspector_data();
            if log::log_enabled!(log::Level::Debug) {
                let summary = data.site.as_ref().map(|s| {
                    format!("{} {} {:?} {:?} {} hosts", s.host, s.ip, s.asn.as_ref().map(|a| (a.number, a.name.as_str())), s.edge, data.hosts.len())
                });
                if summary != self.last_summary {
                    log::debug!("inspector: {summary:?}");
                    self.last_summary = summary;
                }
            }
            let hover = self.cursor.and_then(|(x, y)| self.inspector.hit(x, y));
            self.inspector.set_hover(hover);
            self.inspector.draw(scene, width, height, BAR_Y + BAR_H + 8.0, &data);
        }
        // Prompts hang from under the address bar.
        let current = self.current();
        let spec = current.as_ref().map(|c| self.sheet_spec(c));
        self.sheet.show(spec.as_ref().map(|(key, _)| *key));
        let busy = spec.as_ref().is_some_and(|(_, s)| s.input.as_ref().is_some_and(|i| i.busy));
        moving |= self.sheet.step(dt, busy);
        if let Some((_, spec)) = &spec {
            let omni = self.layout.omnibox;
            self.sheet.draw(scene, spec, omni.center().0, omni.bottom() + 10.0, width);
        }
        if self.app_menu.is_some() {
            let rows = self.app_menu_rows();
            let rows: Vec<Row> = rows.into_iter().map(|(_, r)| r).collect();
            let at = (self.layout.brand.x, self.layout.brand.bottom() + 6.0);
            if let Some(view) = &mut self.app_menu {
                view.draw(scene, &rows, at, (width, height));
            }
        }
        if let Some(owner) = self.menu_owner {
            let r = self.pane_rect(owner);
            let state = self.pane_view(owner).map(|v| v.state.clone());
            match state.as_ref().map(|s| s.borrow()) {
                Some(state) if state.context_menu.is_some() => {
                    let menu = state.context_menu.as_ref().unwrap();
                    self.menu.draw(scene, &menu::rows(menu), (r.x + menu.x, r.y + menu.y), (width, height));
                }
                // The pane went away (tab closed, navigation).
                _ => self.menu_owner = None,
            }
        }
        moving
    }

    fn panel_data(&self) -> PanelData {
        let url = self.page().map(|p| p.state.borrow().url.clone()).unwrap_or_default();
        let (blocked, events) = match self.page() {
            Some(p) => {
                let net = p.net.lock().unwrap();
                (net.blocked, net.events.iter().rev().take(300).cloned().collect())
            }
            None => (0, Vec::new()),
        };
        PanelData {
            site: shield::site_of(&url).unwrap_or_else(|| "start page".into()),
            blocked,
            excepted: self.shield.is_excepted(&url),
            paused: self.shield.is_paused(),
            status: self.shield.status(),
            events,
        }
    }

    fn inspector_data(&self) -> InspectorData {
        let netinfo = self.netinfo.as_deref();
        let lookup = |ip: &str| -> Option<netinfo::Asn> { netinfo?.lookup(ip.parse::<IpAddr>().ok()?) };
        let (site, hosts, events) = match self.page() {
            None => (None, Vec::new(), Vec::new()),
            Some(page) => {
                let inspect = page.inspect.borrow();
                let site = inspect.document.and_then(|i| inspect.requests.get(i)).filter(|r| r.status != 0 || !r.ip.is_empty()).map(|doc| {
                    let asn = lookup(&doc.ip);
                    SiteInfo {
                        host: doc.host.clone(),
                        ip: doc.ip.clone(),
                        port: doc.port,
                        protocol: doc.protocol.clone(),
                        status: doc.status,
                        tls: doc.tls.clone(),
                        edge: netinfo::classify(asn.as_ref().map(|a| a.number), asn.as_ref().map(|a| a.name.as_str()), &doc.headers),
                        server: doc.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("server")).map(|(_, v)| v.clone()),
                        asn,
                    }
                });

                let mut order: Vec<String> = Vec::new();
                let mut rows: HashMap<String, HostRow> = HashMap::new();
                for r in &inspect.requests {
                    if r.host.is_empty() {
                        continue;
                    }
                    let row = rows.entry(r.host.clone()).or_insert_with(|| {
                        order.push(r.host.clone());
                        HostRow {
                            host: r.host.clone(),
                            requests: 0,
                            failed: 0,
                            bytes: 0,
                            ips: Vec::new(),
                            asn: None,
                            edge: netinfo::Edge::Unknown,
                            protocols: Vec::new(),
                            sample: Vec::new(),
                        }
                    });
                    row.requests += 1;
                    row.failed += usize::from(r.failed.is_some());
                    row.bytes += r.bytes;
                    if !r.ip.is_empty() && !row.ips.contains(&r.ip) && row.ips.len() < 3 {
                        row.ips.push(r.ip.clone());
                    }
                    if !r.protocol.is_empty() && !row.protocols.contains(&r.protocol) {
                        row.protocols.push(r.protocol.clone());
                    }
                    if row.sample.len() < 3 {
                        row.sample.push(r.url.clone());
                    }
                    if row.edge == netinfo::Edge::Unknown && (!r.ip.is_empty() || !r.headers.is_empty()) {
                        let asn = lookup(&r.ip);
                        row.edge = netinfo::classify(asn.as_ref().map(|a| a.number), asn.as_ref().map(|a| a.name.as_str()), &r.headers);
                        row.asn = asn;
                    }
                }
                let mut hosts: Vec<HostRow> = order.into_iter().filter_map(|h| rows.remove(&h)).collect();
                hosts.sort_by_key(|h| std::cmp::Reverse(h.requests));
                let events = page.net.lock().unwrap().events.iter().rev().take(500).cloned().collect();
                (site, hosts, events)
            }
        };
        InspectorData {
            site,
            hosts,
            exit: self.vpn.lock().unwrap().clone(),
            lookups: netinfo.map_or(netinfo::Status::Loading, |n| n.status()),
            events,
        }
    }

    fn layout(&mut self, width: f32, height: f32) {
        let new_tab_size = TAB_H;
        let available = width - 2.0 * MARGIN - new_tab_size - TAB_GAP;
        let weight: f32 = self.tabs.iter().map(|t| t.presence.value.max(0.0)).sum();
        let tab_w = if weight * (TAB_W + TAB_GAP) > available {
            (available / weight.max(1.0) - TAB_GAP).max(36.0)
        } else {
            TAB_W
        };

        self.layout.tabs.clear();
        let mut x = MARGIN;
        for tab in &self.tabs {
            let p = tab.presence.value.max(0.0);
            let w = tab_w * p;
            self.layout.tabs.push((tab.id, Rect::new(x, TABS_Y, w, TAB_H)));
            x += w + TAB_GAP * p.min(1.0);
        }
        self.layout.new_tab = Rect::new(x, TABS_Y, new_tab_size, TAB_H);

        let omni_w = (width * 0.42).clamp(280.0, 680.0);
        let omni = Rect::new((width - omni_w) * 0.5, BAR_Y, omni_w, BAR_H);
        self.layout.omnibox = omni;
        self.layout.forward = Rect::new(omni.x - 8.0 - BAR_H, BAR_Y, BAR_H, BAR_H);
        self.layout.back = Rect::new(omni.x - 16.0 - 2.0 * BAR_H, BAR_Y, BAR_H, BAR_H);
        self.layout.extensions = Rect::new(omni.right() + 8.0, BAR_Y, BAR_H + 22.0, BAR_H);
        self.layout.inspector = Rect::new(self.layout.extensions.right() + 8.0, BAR_Y, BAR_H, BAR_H);
        self.layout.brand = Rect::new(MARGIN - 4.0, BAR_Y, LOGO + 112.0, BAR_H);
        let web = self.page().is_some_and(|p| {
            let url = &p.state.borrow().url;
            url.starts_with("https://") || url.starts_with("http://")
        });
        self.layout.star = if web { Rect::new(omni.right() - 40.0, BAR_Y, 34.0, BAR_H) } else { Rect::default() };
        self.layout.key = if self.key_available() { Rect::new(omni.right() - 72.0, BAR_Y, 32.0, BAR_H) } else { Rect::default() };
        self.layout.update = if self.update_ready {
            Rect::new(self.layout.inspector.right() + 8.0, BAR_Y + 6.0, 132.0, BAR_H - 12.0)
        } else {
            Rect::default()
        };
        let content = Rect::new(MARGIN, CONTENT_Y, width - 2.0 * MARGIN, (height - CONTENT_Y - MARGIN).max(0.0));
        self.layout.content = content;
        // DevTools docks to the right of the page.
        let docked = self.active_tab().is_some_and(|t| t.devtools.is_some());
        if docked {
            let dw = (content.w * 0.42).clamp(360.0, (content.w - 320.0).max(360.0));
            self.layout.page = Rect::new(content.x, content.y, (content.w - dw - 8.0).max(0.0), content.h);
            self.layout.devtools = Some(Rect::new(self.layout.page.right() + 8.0, content.y, dw, content.h));
        } else {
            self.layout.page = content;
            self.layout.devtools = None;
        }

        // Start page: recent bookmarks as tiles under the clock.
        self.layout.tiles.clear();
        if self.page().is_none() {
            let profile = self.host.profile.borrow();
            let recent: Vec<(String, String)> =
                profile.bookmarks().iter().rev().take(TILES).map(|b| (b.title.clone(), b.url.clone())).collect();
            let per_row = ((content.w - 80.0) / (TILE_W + 10.0)).floor().clamp(1.0, 4.0) as usize;
            let (cx, cy) = content.center();
            for (i, (_, url)) in recent.into_iter().enumerate() {
                let (row, col) = (i / per_row, i % per_row);
                let in_row = (TILES.min(profile.bookmarks().len()) - row * per_row).min(per_row) as f32;
                let x = cx - (in_row * (TILE_W + 10.0) - 10.0) * 0.5 + col as f32 * (TILE_W + 10.0);
                let y = cy + 70.0 + row as f32 * (TILE_H + 10.0);
                let rect = Rect::new(x, y, TILE_W, TILE_H);
                if rect.bottom() < content.bottom() - 50.0 {
                    self.layout.tiles.push((rect, url));
                }
            }
        }
    }

    fn update_targets(&mut self) {
        if let Some(&(_, rect)) = self.layout.tabs.iter().find(|(id, _)| *id == self.active) {
            self.pill_x.set(rect.x);
            self.pill_w.set(rect.w);
            if !self.pill_placed {
                self.pill_x.snap_to(rect.x);
                self.pill_placed = true;
            }
        }

        let hit = self.hit();
        for tab in &mut self.tabs {
            let hovered = matches!(hit, Some(Hit::Tab(id) | Hit::TabClose(id)) if id == tab.id);
            tab.hover.set(if hovered { 1.0 } else { 0.0 });
        }
        let on = |h: Hit| if hit == Some(h) { 1.0f32 } else { 0.0 };
        self.omnibox_hover.set(on(Hit::Omnibox).max(if self.omnibox.focused { 1.0 } else { 0.0 }));
        self.new_tab_hover.set(on(Hit::NewTab));
        self.extensions_hover.set(on(Hit::Extensions).max(if self.panel.is_open() { 1.0 } else { 0.0 }));
        self.inspector_hover.set(on(Hit::Inspector).max(if self.inspector.is_open() { 1.0 } else { 0.0 }));
        self.star_hover.set(on(Hit::Star));
    }

    // ── drawing ────────────────────────────────────────────────────────────

    fn draw_bar(&self, scene: &mut Scene, width: f32, vpn: &VpnStatus) {
        let cy = BAR_Y + BAR_H * 0.5;
        let brand = self.layout.brand;
        if self.hit() == Some(Hit::Brand) || self.app_menu.is_some() {
            scene.fill(brand, brand.h * 0.5, theme::RAISED);
            scene.stroke(brand, brand.h * 0.5, 1.0, theme::HAIRLINE);
        }
        scene.logo(MARGIN + 6.0, cy - LOGO * 0.5, LOGO, theme::TEXT);
        let wx = MARGIN + 6.0 + LOGO + 8.0;
        let w = scene.text(wx, cy, "unskilled", 16.0, Weight::Medium, theme::TEXT);
        scene.circle(wx + w + 4.0, cy + 4.5, 2.2, theme::ACCENT);

        let state = self.page().map(|p| {
            let s = p.state.borrow();
            (s.url.clone(), s.can_go_back, s.can_go_forward, s.loading)
        });
        let (url, can_back, can_forward, loading) = state.unwrap_or_default();
        // History before/after moves between site sandboxes counts too.
        let (can_back, can_forward) = match self.active_tab() {
            Some(t) => (can_back || !t.back.is_empty(), can_forward || !t.forward.is_empty()),
            None => (can_back, can_forward),
        };

        // Back / forward.
        for (rect, glyph, enabled) in [(self.layout.back, "←", can_back), (self.layout.forward, "→", can_forward)] {
            scene.stroke(rect, rect.h * 0.5, 1.0, theme::HAIRLINE);
            let color = if enabled { theme::TEXT_DIM } else { theme::TEXT_FAINT.with_alpha(0.5) };
            let (gx, gy) = rect.center();
            scene.text_centered(gx, gy, glyph, UI_TEXT + 1.0, Weight::Regular, color);
        }

        // Omnibox.
        let omni = self.layout.omnibox;
        let h = self.omnibox_hover.value;
        scene.fill(omni, omni.h * 0.5, theme::RAISED);
        scene.stroke(omni, omni.h * 0.5, 1.0, theme::HAIRLINE.lerp(theme::HAIRLINE_HOVER, h));
        self.draw_load_ring(scene, omni);
        let secure = url.starts_with("https://") || url.starts_with("unskilled://");
        let dot_color = if url.is_empty() || secure { theme::TEXT } else { theme::ACCENT };
        if loading {
            // Loading: the dot breathes.
            let pulse = 0.35 + 0.65 * (0.5 + 0.5 * (self.spin * std::f32::consts::TAU).cos());
            scene.circle(omni.x + 20.0, cy, 3.0, dot_color.with_alpha(pulse));
        } else {
            scene.circle(omni.x + 20.0, cy, 3.0, dot_color);
        }
        let text_x = omni.x + 34.0;
        let icons = self.layout.star.w + self.layout.key.w;
        let text_w = omni.w - 54.0 - icons;
        // Bookmark star and, when a saved login fits the page, the key.
        if self.layout.star.w > 0.0 && !self.omnibox.focused {
            let on = self.host.profile.borrow().is_bookmarked(&url);
            let (sx, sy) = self.layout.star.center();
            let color = if on { theme::TEXT } else { theme::TEXT_FAINT.lerp(theme::TEXT_DIM, self.star_hover.value) };
            let pitch = 2.7 * self.star_pop.value;
            let ribbon = if on { &RIBBON_FILLED } else { &RIBBON };
            dot_icon(scene, sx, sy, ribbon, pitch, 1.0 * self.star_pop.value.min(1.3), color);
        }
        if self.layout.key.w > 0.0 && !self.omnibox.focused {
            let (kx, ky) = self.layout.key.center();
            let hovered = self.hit() == Some(Hit::Key);
            dot_icon(scene, kx, ky, &KEY, 3.0, 1.1, if hovered { theme::TEXT } else { theme::TEXT_DIM });
        }
        if self.omnibox.focused {
            let text = scene.text.fit_tail(&self.omnibox.text, UI_TEXT, Weight::Regular, text_w - 10.0);
            let tw = scene.text.measure(&text, UI_TEXT, Weight::Regular);
            if self.omnibox.selected && !text.is_empty() {
                scene.fill(Rect::new(text_x - 4.0, cy - 10.0, tw + 8.0, 20.0), 4.0, theme::TEXT);
                scene.text(text_x, cy, &text, UI_TEXT, Weight::Regular, theme::ON_LIGHT);
            } else {
                scene.text(text_x, cy, &text, UI_TEXT, Weight::Regular, theme::TEXT);
                // Solid red block caret, like the terminal's.
                scene.fill(Rect::new(text_x + tw + 1.0, cy - 8.0, 7.0, 16.0), 1.0, theme::ACCENT);
            }
        } else if url.is_empty() {
            let hint = scene.text.fit("Search or enter address", UI_TEXT, Weight::Regular, text_w);
            scene.text(text_x, cy, &hint, UI_TEXT, Weight::Regular, theme::TEXT_FAINT.lerp(theme::TEXT_DIM, h));
        } else {
            let shown = display_url(&url);
            let text = scene.text.fit(&shown, UI_TEXT, Weight::Regular, text_w);
            scene.text(text_x, cy, &text, UI_TEXT, Weight::Regular, theme::TEXT_DIM.lerp(theme::TEXT, h));
        }

        // Extensions button: a 2×2 dot grid + this page's blocked count.
        let ext = self.layout.extensions;
        let eh = self.extensions_hover.value;
        scene.fill(ext, ext.h * 0.5, theme::RAISED.with_alpha(eh));
        scene.stroke(ext, ext.h * 0.5, 1.0, theme::HAIRLINE.lerp(theme::HAIRLINE_HOVER, eh));
        let off = self.shield.is_paused() || self.shield.is_excepted(&url);
        let grid = if off { theme::ACCENT } else { theme::TEXT };
        let gx = ext.x + 16.0;
        for (dx, dy) in [(0.0, 0.0), (7.0, 0.0), (0.0, 7.0), (7.0, 7.0)] {
            scene.circle(gx + dx, cy - 3.5 + dy, 2.2, grid);
        }
        let blocked = self.page().map_or(0, |p| p.net.lock().unwrap().blocked);
        let count = if blocked > 999 { "999+".to_owned() } else { blocked.to_string() };
        scene.text(gx + 15.0, cy, &count, SMALL_TEXT, Weight::Regular, theme::TEXT_DIM);

        // Mullvad: a ring of dots. White = tunnel up, red = down, spinning = connecting.
        let ring_cx = width - MARGIN - 16.0;
        let (ring_color, label) = match vpn {
            VpnStatus::Connected { relay, .. } => (theme::TEXT, short_relay(relay)),
            VpnStatus::Connecting => (theme::TEXT_DIM, "connecting".to_owned()),
            VpnStatus::Disconnected => (theme::ACCENT, "unprotected".to_owned()),
            VpnStatus::Missing => (theme::ACCENT, "no mullvad".to_owned()),
            VpnStatus::Unknown => (theme::TEXT_FAINT, String::new()),
        };
        const DOTS: usize = 10;
        for i in 0..DOTS {
            let a = i as f32 / DOTS as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let color = if *vpn == VpnStatus::Connecting {
                let phase = (i as f32 / DOTS as f32 - self.spin).rem_euclid(1.0);
                theme::TEXT_FAINT.lerp(theme::TEXT, 1.0 - phase)
            } else {
                ring_color
            };
            scene.circle(ring_cx + a.cos() * 9.0, cy + a.sin() * 9.0, 1.6, color);
        }
        if matches!(vpn, VpnStatus::Connected { .. }) {
            scene.circle(ring_cx, cy, 2.4, theme::TEXT);
        }
        // Inspector button.
        let ins = self.layout.inspector;
        let ih = self.inspector_hover.value;
        scene.fill(ins, ins.h * 0.5, theme::RAISED.with_alpha(ih));
        scene.stroke(ins, ins.h * 0.5, 1.0, theme::HAIRLINE.lerp(theme::HAIRLINE_HOVER, ih));
        scene.text_centered(ins.center().0, cy, "</>", SMALL_TEXT, Weight::Regular, theme::TEXT_DIM.lerp(theme::TEXT, ih));

        // Update pill: a newer build is installed.
        let mut bar_right = ins.right();
        if self.update_ready {
            let u = self.layout.update;
            let hovered = self.hit() == Some(Hit::Update);
            scene.fill(u, u.h * 0.5, if hovered { theme::TEXT } else { theme::RAISED });
            scene.stroke(u, u.h * 0.5, 1.0, theme::HAIRLINE_HOVER);
            scene.circle(u.x + 13.0, cy, 3.0, theme::ACCENT);
            let color = if hovered { theme::ON_LIGHT } else { theme::TEXT };
            scene.text(u.x + 24.0, cy, "update · restart", SMALL_TEXT, Weight::Regular, color);
            bar_right = u.right();
        }

        let room = ring_cx - 22.0 - bar_right - 12.0;
        let label = scene.text.fit(&label, UI_TEXT, Weight::Regular, room.max(0.0));
        scene.text_right(ring_cx - 20.0, cy, &label, UI_TEXT, Weight::Regular, theme::TEXT_DIM);
    }

    /// Loading, drawn into the address bar's own outline: dots light up from the left along the
    /// top and bottom edges at once, each led by a red head, meeting on the right when the page
    /// is done — then the ring fades.
    fn draw_load_ring(&self, scene: &mut Scene, omni: Rect) {
        let alpha = self.load_alpha.value.clamp(0.0, 1.0);
        if alpha < 0.01 {
            return;
        }
        let p = self.load_progress.value.clamp(0.0, 1.0);
        let r = omni.h * 0.5 - 0.5;
        let cy = omni.center().1;
        let (left, right) = (omni.x + omni.h * 0.5, omni.right() - omni.h * 0.5);
        let straight = right - left;
        let quarter = std::f32::consts::FRAC_PI_2 * r;
        let half = 2.0 * quarter + straight;
        // Distance along the top half (left middle → right middle) → point.
        let point = |d: f32| -> (f32, f32) {
            if d < quarter {
                let a = d / r;
                (left - r * a.cos(), cy - r * a.sin())
            } else if d < quarter + straight {
                (left + d - quarter, cy - r)
            } else {
                let a = ((d - quarter - straight) / r).min(std::f32::consts::FRAC_PI_2);
                (right + r * a.sin(), cy - r * a.cos())
            }
        };
        const PITCH: f32 = 5.0;
        let head = p * half;
        let wave = self.spin * std::f32::consts::TAU;
        let mut d = 0.0;
        while d <= head {
            let trail = (-(head - d) / 110.0).exp();
            let shimmer = 0.9 + 0.1 * (d * 0.06 - wave).sin();
            let a = alpha * (0.2 + 0.8 * trail) * shimmer;
            let (x, y) = point(d);
            scene.circle(x, y, 1.05, theme::TEXT.with_alpha(a));
            scene.circle(x, 2.0 * cy - y, 1.05, theme::TEXT.with_alpha(a));
            d += PITCH;
        }
        if p < 0.995 {
            let (x, y) = point(head);
            for y in [y, 2.0 * cy - y] {
                scene.glow(Rect::new(x - 1.5, y - 1.5, 3.0, 3.0), 1.5, 7.0, theme::ACCENT.with_alpha(alpha * 0.55));
                scene.circle(x, y, 1.7, theme::ACCENT.with_alpha(alpha));
            }
        }
    }

    fn tab_title(&self, tab: &Tab) -> String {
        match &tab.page {
            None => "new tab".into(),
            Some(p) => {
                let s = p.state.borrow();
                if !s.title.is_empty() && !s.title.starts_with("http") {
                    s.title.clone()
                } else {
                    shield::site_of(&s.url).unwrap_or_else(|| "loading".into())
                }
            }
        }
    }

    fn draw_tabs(&self, scene: &mut Scene) {
        let label_y = TABS_Y + TAB_H * 0.5;
        let rects: Vec<(&Tab, Rect)> = self.tabs.iter().zip(self.layout.tabs.iter().map(|&(_, r)| r)).collect();

        // Pass 1: outlines and light labels.
        for (tab, rect) in &rects {
            let p = tab.presence.value.clamp(0.0, 1.0);
            let hover = tab.hover.value;
            scene.fill(*rect, TAB_H * 0.5, theme::RAISED.with_alpha(hover * p));
            scene.stroke(*rect, TAB_H * 0.5, 1.0, theme::HAIRLINE.lerp(theme::HAIRLINE_HOVER, hover).with_alpha(p));
            let color = theme::TEXT_DIM.lerp(theme::TEXT, hover).with_alpha(p);
            self.draw_tab_label(scene, tab, *rect, label_y, color);
        }

        // Pass 2: the gliding pill, with dark labels masked to it.
        let pill = Rect::new(self.pill_x.value, TABS_Y, self.pill_w.value.max(0.0), TAB_H);
        scene.fill(pill, TAB_H * 0.5, theme::TEXT);
        scene.clipped(pill, |scene| {
            for (tab, rect) in &rects {
                let p = tab.presence.value.clamp(0.0, 1.0);
                self.draw_tab_label(scene, tab, *rect, label_y, theme::ON_LIGHT.with_alpha(p));
            }
        });

        let b = self.layout.new_tab;
        let h = self.new_tab_hover.value;
        scene.fill(b, TAB_H * 0.5, theme::RAISED.with_alpha(h));
        scene.stroke(b, TAB_H * 0.5, 1.0, theme::HAIRLINE.lerp(theme::HAIRLINE_HOVER, h));
        let (cx, cy) = b.center();
        scene.text_centered(cx, cy, "+", UI_TEXT + 2.0, Weight::Regular, theme::TEXT_DIM.lerp(theme::TEXT, h));
    }

    fn draw_tab_label(&self, scene: &mut Scene, tab: &Tab, rect: Rect, cy: f32, color: Color) {
        let show_close = tab.hover.value.max(if tab.id == self.active { 1.0 } else { 0.0 });
        let loading = tab.page.as_ref().is_some_and(|p| p.state.borrow().loading);
        let close_room = 24.0;
        let title = self.tab_title(tab);
        scene.clipped(rect.inset(1.0), |scene| {
            let mut x = rect.x + 14.0;
            if loading {
                // Three dots chasing each other.
                for i in 0..3 {
                    let phase = (self.spin * 3.0 - i as f32 * 0.35).rem_euclid(3.0);
                    let a = if phase < 1.0 { 1.0 - phase * 0.7 } else { 0.3 };
                    scene.circle(x + i as f32 * 5.0 + 1.5, cy, 1.4, color.with_alpha(a));
                }
                x += 20.0;
            }
            let text = scene.text.fit(&title, TAB_TEXT, Weight::Regular, rect.right() - x - 10.0 - close_room);
            scene.text(x, cy, &text, TAB_TEXT, Weight::Regular, color);
            if rect.w > 60.0 {
                scene.text_right(rect.right() - 12.0, cy, "×", UI_TEXT + 1.0, Weight::Regular, color.with_alpha(show_close * 0.8));
            }
        });
    }

    fn draw_content(&self, scene: &mut Scene) {
        let card = self.layout.content;
        if card.h <= 0.0 {
            return;
        }
        let page_card = if self.page().is_some() { self.layout.page } else { card };
        scene.fill(page_card, CONTENT_RADIUS, theme::SURFACE);
        scene.stroke(page_card, CONTENT_RADIUS, 1.0, theme::HAIRLINE.with_alpha(0.7));

        let fade = self.content_fade.value.clamp(0.0, 1.0);
        let dx = self.content_shift.value;

        if self.page().is_some() {
            for pane in [Pane::Page, Pane::DevTools] {
                let Some(view) = self.pane_view(pane) else { continue };
                let r = self.pane_rect(pane);
                if pane == Pane::DevTools {
                    // DevTools gets its own card next to the page's.
                    scene.fill(r, CONTENT_RADIUS, theme::SURFACE);
                    scene.stroke(r, CONTENT_RADIUS, 1.0, theme::HAIRLINE.with_alpha(0.7));
                }
                let Some(frame) = view.state.borrow().frame.clone() else { continue };
                // Snap to physical pixels when at rest so text stays crisp.
                let s = self.scale;
                let x = if dx.abs() < 0.01 { (r.x * s).round() / s } else { r.x + dx };
                let size = frame.size();
                let rect = Rect::new(x, (r.y * s).round() / s, size.width as f32 / s, size.height as f32 / s);
                scene.clipped(r, |scene| scene.image(rect, CONTENT_RADIUS, fade, &frame));
            }
            return;
        }

        let (cx, cy) = card.center();
        let now = clock::now();
        let hovered = match self.hit() {
            Some(Hit::Tile(i)) => Some(i),
            _ => None,
        };
        let profile = self.host.profile.borrow();
        scene.clipped(card.inset(1.0), |scene| {
            for (i, (rect, url)) in self.layout.tiles.iter().enumerate() {
                let rect = Rect::new(rect.x + dx, rect.y, rect.w, rect.h);
                let on = hovered == Some(i);
                scene.fill(rect, 14.0, theme::RAISED.with_alpha(fade));
                scene.stroke(rect, 14.0, 1.0, if on { theme::HAIRLINE_HOVER } else { theme::HAIRLINE }.with_alpha(fade));
                let title = profile.bookmarks().iter().find(|b| &b.url == url).map_or(String::new(), |b| b.title.clone());
                let site = display_url(url).split('/').next().unwrap_or_default().to_owned();
                let title = if title.is_empty() { site.clone() } else { title };
                let t = scene.text.fit(&title, 12.5, Weight::Regular, rect.w - 28.0);
                scene.text(rect.x + 14.0, rect.y + 19.0, &t, 12.5, Weight::Regular, if on { theme::TEXT } else { theme::TEXT_DIM }.with_alpha(fade));
                let s = scene.text.fit(&site, SMALL_TEXT, Weight::Regular, rect.w - 28.0);
                scene.text(rect.x + 14.0, rect.y + 36.0, &s, SMALL_TEXT, Weight::Regular, theme::TEXT_FAINT.with_alpha(fade));
            }
            // Start page: a big, thin clock (lifted when bookmarks sit under it).
            let cy = if self.layout.tiles.is_empty() { cy } else { cy - 40.0 };
            let size = (card.w / 7.5).clamp(64.0, 132.0);
            let time = format!("{:02}:{:02}", now.hour, now.minute);
            scene.text_centered(cx + dx, cy - size * 0.35, &time, size, Weight::Light, theme::TEXT.with_alpha(fade));
            scene.text_centered(cx + dx, cy + size * 0.35, &now.date_label(), UI_TEXT + 1.0, Weight::Regular, theme::TEXT_DIM.with_alpha(fade));

            let hint = "ctrl+l address   ctrl+t new tab   ctrl+d bookmark   ctrl+, settings";
            scene.text_centered(cx, card.bottom() - 30.0, hint, SMALL_TEXT, Weight::Regular, theme::TEXT_FAINT.with_alpha(fade));
        });
    }
}

/// Draws a dot-matrix `pattern` centred on (cx, cy).
fn dot_icon(scene: &mut Scene, cx: f32, cy: f32, pattern: &[&str], pitch: f32, radius: f32, color: Color) {
    let rows = pattern.len() as f32;
    let cols = pattern.iter().map(|r| r.len()).max().unwrap_or(0) as f32;
    for (j, row) in pattern.iter().enumerate() {
        for (i, c) in row.bytes().enumerate() {
            if c == b'#' {
                let x = cx + (i as f32 - (cols - 1.0) * 0.5) * pitch;
                let y = cy + (j as f32 - (rows - 1.0) * 0.5) * pitch;
                scene.circle(x, y, radius, color);
            }
        }
    }
}

/// Typed text → URL: keeps explicit URLs, adds https:// to hostnames, searches everything else
/// with `search` (the engine's query prefix).
fn to_url(input: &str, search: &str) -> String {
    let t = input.trim();
    if t.contains("://") || t.starts_with("about:") || t.starts_with("data:") {
        return t.to_owned();
    }
    let host = t.split(['/', '?', '#']).next().unwrap_or_default();
    if !t.contains(' ') && (host.contains('.') || host.starts_with("localhost")) {
        return format!("https://{t}");
    }
    let mut q = String::new();
    for b in t.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => q.push(b as char),
            b' ' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    format!("{search}{q}")
}

/// `https://www.example.com/path/` → `example.com/path`.
fn display_url(url: &str) -> String {
    if url.starts_with("unskilled://") {
        return url.strip_suffix('/').unwrap_or(url).to_owned();
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.strip_suffix('/').unwrap_or(rest).to_owned()
}

/// `bg-sof-wg-001` → `bg-sof`.
fn short_relay(relay: &str) -> String {
    relay.splitn(3, '-').take(2).collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_input_to_url() {
        let ddg = "https://duckduckgo.com/?q=";
        assert_eq!(to_url("github.com", ddg), "https://github.com");
        assert_eq!(to_url("http://x.org/a", ddg), "http://x.org/a");
        assert_eq!(to_url("localhost:8080", ddg), "https://localhost:8080");
        assert_eq!(to_url("unskilled://settings", ddg), "unskilled://settings");
        assert_eq!(to_url("rust spring physics", ddg), "https://duckduckgo.com/?q=rust+spring+physics");
        assert_eq!(to_url("c++", "https://leta.mullvad.net/search?q="), "https://leta.mullvad.net/search?q=c%2B%2B");
    }

    #[test]
    fn url_display() {
        assert_eq!(display_url("https://www.youtube.com/"), "youtube.com");
        assert_eq!(display_url("https://github.com/a/b"), "github.com/a/b");
        assert_eq!(display_url("unskilled://settings"), "unskilled://settings");
    }
}
