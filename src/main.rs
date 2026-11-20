#![windows_subsystem = "windows"]

mod capture;
mod psn;
mod remote;
mod theme;
mod video;
mod widgets;

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect};
use eframe::egui_wgpu::{self, wgpu};
use std::sync::Arc;
use std::time::{Duration, Instant};
use theme::*;
use video::{FrameSlot, Look, Scaler};

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Source {
    RemotePlay,
    Capture,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Mode {
    Responsive,
    Smooth,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Res {
    P1080,
    P720,
}

/// How finished frames reach the screen: the trade between delay, tearing and smoothness.
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Present {
    LowDelay,
    Tearing,
    Vsync,
}

/// Everything nagito remembers between launches.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    source: Source,
    mode: Mode,
    scaler: Scaler,
    sharpness: f32,
    video: Option<String>,
    capture_mode: Option<capture::CamMode>,
    audio: Option<String>,
    res: Res,
    bitrate_mbps: u32,
    fps: u32,
    hevc: bool,
    present: Present,
    start_fullscreen: bool,
    show_stats: bool,
    volume: f32,
    rumble: bool,
    triggers: bool,
    motion: bool,
    touchpad: bool,
    haptics: bool,
    mic: bool,
    button_map: [u8; 16],
    kbm: bool,
    mouse_sens: f32,
    keys: Vec<String>,
    account_id: String,
    consoles: Vec<remote::Linked>,
    console: Option<String>,
}

impl Settings {
    fn quality(&self) -> remote::Quality {
        let (width, height) = match self.res {
            Res::P1080 => (1920, 1080),
            Res::P720 => (1280, 720),
        };
        remote::Quality {
            width,
            height,
            fps: self.fps as i32,
            bitrate_kbps: self.bitrate_mbps as i32 * 1000,
            hevc: self.hevc,
            rumble: self.rumble,
            triggers: self.triggers,
            motion: self.motion,
            touchpad: self.touchpad,
            haptics: self.haptics,
            mic: self.mic,
            button_map: self.button_map,
        }
    }

    fn key(&self, action: usize) -> Option<egui::Key> {
        self.keys.get(action).and_then(|k| egui::Key::from_name(k))
    }
}

/// Keyboard actions: the 16 PS5 buttons (remote::BUTTONS order), then these.
const KB_EXTRA: [&str; 6] = ["L2", "R2", "Move forward", "Move back", "Move left", "Move right"];
const KB_DEFAULT: [&str; 22] = [
    "Space", "C", "R", "F", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Q", "E", "X", "B", "Enter", "Backspace", "T", "Home", "", "", "W", "S", "A", "D",
];

fn kb_action_name(i: usize) -> &'static str {
    remote::BUTTONS.get(i).copied().unwrap_or_else(|| KB_EXTRA[i - 16])
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source: Source::Capture,
            mode: Mode::Responsive,
            scaler: Scaler::Fsr,
            sharpness: 0.5,
            video: None,
            capture_mode: None,
            audio: None,
            res: Res::P1080,
            bitrate_mbps: 50, // wired LAN handles this easily; 15 is chiaki's Wi-Fi-safe default
            fps: 60,
            hevc: true,
            present: Present::LowDelay,
            start_fullscreen: false,
            show_stats: true,
            volume: 1.0,
            rumble: true,
            triggers: true,
            motion: true,
            touchpad: true,
            haptics: true,
            mic: true,
            button_map: std::array::from_fn(|i| i as u8),
            kbm: false,
            mouse_sens: 1.0,
            keys: KB_DEFAULT.iter().map(|k| k.to_string()).collect(),
            account_id: String::new(),
            consoles: vec![],
            console: None,
        }
    }
}

type LiveKey = (capture::VideoDevice, Option<capture::CamMode>, Option<String>);

enum Session {
    Capture(capture::Session),
    Remote(remote::Stream),
}

/// What is producing frames right now. Capture runs on the launcher too, so preview and play share it.
struct Live {
    key: Option<LiveKey>,
    session: Session,
    slot: Arc<FrameSlot>,
}

/// One console as the launcher shows it: found on the network, linked, or both.
#[derive(Clone)]
struct Console {
    id: String,
    name: String,
    addr: String,
    ps5: bool,
    app: String,
    power: Option<remote::Power>,
    linked: Option<remote::Linked>,
}

struct LinkForm {
    console: Console,
    pin: String,
    running: Option<remote::Link>,
    error: Option<String>,
}

/// Hosting side of an invite: router setup runs in the background, then the link is shown.
struct Share {
    console: remote::Linked,
    job: Arc<std::sync::Mutex<Option<(i32, String)>>>,
    ports: Option<i32>,
    addr: String,
}

enum Action {
    Play,
    Wake,
    Link,
    Nothing,
}

struct App {
    s: Settings,
    videos: Vec<capture::VideoDevice>,
    audios: Vec<String>,
    video: Option<capture::VideoDevice>,
    live: Option<Live>,
    playing: bool,
    settings_open: bool,
    discovery: Option<remote::Discovery>,
    discovery_failed: bool,
    link: Option<LinkForm>,
    notice: Option<(String, bool, Instant)>,
    psn_redirect: String,
    psn_job: Option<Arc<std::sync::Mutex<Option<Result<String, String>>>>>,
    pin_entry: String,
    invite_entry: String,
    share: Option<Share>,
    discovery_extra: Vec<String>,
    rebinding: Option<usize>,
    mouse_stick: egui::Vec2,
    grabbed: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        theme::install(&cc.egui_ctx);
        let rs = cc.wgpu_render_state.as_ref().expect("nagito needs the wgpu renderer");
        video::install(rs);
        let mut s: Settings = cc.storage.and_then(|st| eframe::get_value(st, "nagito")).unwrap_or_default();
        if std::env::var_os("NAGITO_SCREENSHOT_REMOTE").is_some() {
            s.source = Source::RemotePlay;
        }
        let mut app = Self {
            s,
            videos: vec![],
            audios: vec![],
            video: None,
            live: None,
            playing: false,
            settings_open: std::env::var_os("NAGITO_SCREENSHOT_SETTINGS").is_some(),
            discovery: None,
            discovery_failed: false,
            link: None,
            notice: None,
            psn_redirect: String::new(),
            psn_job: None,
            pin_entry: String::new(),
            invite_entry: String::new(),
            share: None,
            discovery_extra: vec![],
            rebinding: None,
            mouse_stick: egui::Vec2::ZERO,
            grabbed: false,
        };
        if app.s.keys.len() != KB_DEFAULT.len() {
            app.s.keys = KB_DEFAULT.iter().map(|k| k.to_string()).collect();
        }
        app.rescan();
        if std::env::var_os("NAGITO_TEST_PATTERN").is_some() {
            // Dev aid: play a synthetic 30 fps source with frame generation, no console or camera needed.
            let slot = Arc::new(FrameSlot::default());
            let c = cc.egui_ctx.clone();
            let session = capture::test_pattern(slot.clone(), move || c.request_repaint());
            app.live = Some(Live { key: None, session: Session::Capture(session), slot });
            app.playing = true;
            app.s.mode = Mode::Smooth;
            app.s.show_stats = true;
        }
        app
    }

    fn look(&self) -> Look {
        Look { scaler: self.s.scaler, sharpness: self.s.sharpness, frame_gen: self.s.mode == Mode::Smooth }
    }

    fn rescan(&mut self) {
        self.videos = capture::video_devices();
        self.audios = capture::audio_inputs();
        let saved = self.s.video.clone();
        self.video = self.videos.iter().find(|v| Some(&v.name) == saved.as_ref()).or(self.videos.first()).cloned();
        if self.s.audio.as_ref().is_none_or(|a| !self.audios.contains(a)) {
            self.s.audio = self.video.as_ref().and_then(|v| capture::guess_audio(&v.name, &self.audios));
        }
        self.live = None; // reopen the device after a rescan
    }

    /// Keep the capture session matching the chosen devices; a Remote Play stream only lives while playing.
    fn sync_live(&mut self, ctx: &egui::Context) {
        if self.s.source == Source::RemotePlay {
            if !self.playing || self.live.as_ref().is_some_and(|l| l.key.is_some()) {
                self.live = None;
            }
            let extra: Vec<String> = self.s.consoles.iter().filter(|l| l.shared).map(|l| l.addr.clone()).collect();
            if extra != self.discovery_extra {
                self.discovery = None; // restart so shared consoles get pinged too
                self.discovery_failed = false;
                self.discovery_extra = extra;
            }
            if self.discovery.is_none() && !self.discovery_failed {
                let c = ctx.clone();
                self.discovery = remote::Discovery::start(&self.discovery_extra, move || c.request_repaint());
                self.discovery_failed = self.discovery.is_none();
            }
            return;
        }
        let want: Option<LiveKey> = self.video.clone().map(|v| (v, self.s.capture_mode, self.s.audio.clone()));
        if self.live.as_ref().and_then(|l| l.key.as_ref()) == want.as_ref() && self.live.is_some() == want.is_some() {
            return;
        }
        self.live = None; // stop the old device before opening the new one
        self.live = want.map(|key| {
            let slot = Arc::new(FrameSlot::default());
            let c = ctx.clone();
            let session = capture::start(key.0.clone(), key.1, key.2.clone(), slot.clone(), move || c.request_repaint());
            Live { key: Some(key), session: Session::Capture(session), slot }
        });
    }

    fn stats(&self) -> Option<video::Stats> {
        self.live.as_ref().map(|l| l.slot.stats.lock().unwrap().clone())
    }

    fn signal_ok(&self) -> bool {
        self.stats().is_some_and(|s| s.width > 0 && s.error.is_none())
    }

    fn consoles(&self) -> Vec<Console> {
        let found = self.discovery.as_ref().map(|d| d.hosts()).unwrap_or_default();
        let mut list: Vec<Console> = found
            .into_iter()
            .map(|h| Console {
                linked: self.s.consoles.iter().find(|l| l.host_id == h.id).cloned(),
                id: h.id,
                name: h.name,
                addr: h.addr,
                ps5: h.ps5,
                app: h.app,
                power: Some(h.power),
            })
            .collect();
        for l in &self.s.consoles {
            if !list.iter().any(|c| c.id == l.host_id) {
                list.push(Console { id: l.host_id.clone(), name: l.name.clone(), addr: l.addr.clone(), ps5: l.ps5, app: String::new(), power: None, linked: Some(l.clone()) });
            }
        }
        list
    }

    fn selected_console(&self) -> Option<Console> {
        let list = self.consoles();
        list.iter().find(|c| Some(&c.id) == self.s.console.as_ref()).or(list.first()).cloned()
    }

    fn action(&self) -> (Action, &'static str) {
        match self.s.source {
            Source::Capture if self.signal_ok() => (Action::Play, "Start playing"),
            Source::Capture => (Action::Nothing, "Start playing"),
            Source::RemotePlay => match self.selected_console() {
                None => (Action::Nothing, "Start playing"),
                Some(c) if c.linked.is_none() => (Action::Link, "Link this console"),
                Some(c) if c.power == Some(remote::Power::Resting) => (Action::Wake, "Wake up console"),
                Some(c) if c.power == Some(remote::Power::Ready) => (Action::Play, "Start playing"),
                // a friend's console may not answer pings through their router, so let the user try anyway
                Some(c) if c.linked.as_ref().is_some_and(|l| l.shared) => (Action::Play, "Connect over the internet"),
                Some(_) => (Action::Nothing, "Start playing"),
            },
        }
    }

    fn notify(&mut self, text: impl Into<String>, ok: bool) {
        self.notice = Some((text.into(), ok, Instant::now()));
    }

    fn run_action(&mut self, ctx: &egui::Context) {
        match self.action().0 {
            Action::Nothing => {}
            Action::Link => {
                if let Some(c) = self.selected_console() {
                    self.link = Some(LinkForm { console: c, pin: String::new(), running: None, error: None });
                }
            }
            Action::Wake => {
                let Some(l) = self.selected_console().and_then(|c| c.linked) else { return };
                if remote::wake(&l) {
                    self.notify("Wake-up sent. The PS5 takes about 20 seconds to be ready.", true);
                } else {
                    self.notify("Could not reach the console to wake it.", false);
                }
            }
            Action::Play => {
                if self.s.source == Source::Capture {
                    self.play(ctx, true);
                    return;
                }
                let owner = self.selected_console().and_then(|c| c.linked).and_then(|l| l.account_id);
                let Some(account) = remote::parse_account_id(owner.as_deref().unwrap_or(&self.s.account_id)) else {
                    self.notify("Add your PSN account ID in the sidebar first.", false);
                    return;
                };
                let Some(mut linked) = self.selected_console().and_then(|c| c.linked.map(|l| (l, c.addr))).map(|(mut l, a)| {
                    l.addr = a; // the console may have a new IP since linking
                    l
                }) else {
                    return;
                };
                linked.addr = linked.addr.trim().to_owned();
                let slot = Arc::new(FrameSlot::default());
                let c = ctx.clone();
                match remote::Stream::start(&linked, account, &self.s.quality(), slot.clone(), move || c.request_repaint()) {
                    Ok(stream) => {
                        self.live = Some(Live { key: None, session: Session::Remote(stream), slot });
                        self.play(ctx, true);
                    }
                    Err(e) => self.notify(e, false),
                }
            }
        }
    }

    fn play(&mut self, ctx: &egui::Context, on: bool) {
        self.playing = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on && (self.s.mode == Mode::Smooth || self.s.start_fullscreen)));
    }

    fn mode_note(&self) -> String {
        match self.s.mode {
            Mode::Responsive => "Each frame is shown the moment it arrives. Lowest delay.".into(),
            Mode::Smooth => "nagito draws a frame between every two it gets, so a 30 fps stream plays at 60. Adds half a frame of delay (17 ms at 30 fps); fast motion can wobble a little at edges.".into(),
        }
    }

    fn launcher(&mut self, ui: &mut egui::Ui, full: Rect) {
        let ctx = ui.ctx().clone();
        let m = (full.width() * 0.03).clamp(24.0, 48.0);
        let inner = full.shrink(m);

        let word = ui.painter().text(inner.left_top(), Align2::LEFT_TOP, "nagito", display(34.0), PAPER);
        let btn = Rect::from_min_size(pos2(word.right() + 24.0, inner.top() + 3.0), vec2(104.0, 34.0));
        let label = if self.settings_open { "Done" } else { "Settings" };
        if ui.put(btn, egui::Button::new(label).corner_radius(17.0)).clicked() {
            self.settings_open = !self.settings_open;
        }
        let (status, ok) = self.status_line();
        widgets::status_chip(ui, pos2(inner.right(), inner.top() + 20.0), &status, ok);

        let body = Rect::from_min_max(pos2(inner.left(), inner.top() + 72.0), inner.max);
        let side_w = (body.width() * 0.32).clamp(340.0, 420.0);
        let side = Rect::from_min_max(pos2(body.right() - side_w, body.top()), body.max);
        let stage = Rect::from_min_max(body.min, pos2(side.left() - m, body.bottom()));

        if self.settings_open {
            self.settings_stage(ui, stage);
        } else if self.link.is_some() {
            self.link_stage(ui, stage);
        } else {
            self.stage(ui, stage);
        }
        self.sidebar(ui, side, &ctx);

        if let Some((text, good, at)) = &self.notice {
            if at.elapsed() < Duration::from_secs(6) {
                let r = Rect::from_center_size(pos2(stage.center().x, stage.bottom() - 70.0), vec2((stage.width() - 48.0).min(560.0), 44.0));
                ui.painter().rect_filled(r, 12.0, PANEL_HI);
                ui.painter().circle_filled(pos2(r.left() + 20.0, r.center().y), 4.5, if *good { SIGNAL } else { FLARE });
                ui.painter().text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(15.0), PAPER);
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }

        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && self.link.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.run_action(&ctx);
        }
    }

    fn status_line(&self) -> (String, bool) {
        if self.s.source == Source::RemotePlay {
            if self.discovery_failed {
                return ("Remote Play could not start its network layer".into(), false);
            }
            return match self.selected_console() {
                None => ("Looking for consoles on your network".into(), false),
                Some(c) => match (&c.linked, &c.power) {
                    (None, _) => (format!("{} is not linked yet", c.name), false),
                    (_, Some(remote::Power::Ready)) => (format!("{} is ready", c.name), true),
                    (_, Some(remote::Power::Resting)) => (format!("{} is resting", c.name), false),
                    (Some(l), None) if l.shared => (format!("{} is shared over the internet", c.name), true),
                    _ => (format!("{} is not on your network", c.name), false),
                },
            };
        }
        match self.stats() {
            None => ("No capture card connected".into(), false),
            Some(s) if s.error.is_some() => ("Capture card stopped".into(), false),
            Some(s) if s.width == 0 => ("Waiting for picture".into(), false),
            Some(s) => (format!("{}×{} at {:.0} fps", s.width, s.height, s.source_fps), true),
        }
    }

    /// Big centered message inside the stage screen.
    fn stage_message(p: &egui::Painter, screen: Rect, title: &str, hint: &str) {
        p.text(screen.center() - vec2(0.0, 18.0), Align2::CENTER_CENTER, title, display(40.0), PAPER);
        let g = p.layout(hint.to_owned(), FontId::proportional(16.0), HAZE, (screen.width() * 0.6).min(520.0));
        p.galley(screen.center() + vec2(-g.size().x / 2.0, 14.0), g, HAZE);
    }

    fn screen_rect(area: Rect) -> Rect {
        Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - 40.0))
    }

    /// The big preview of what will be on screen.
    fn stage(&mut self, ui: &mut egui::Ui, area: Rect) {
        let screen = Self::screen_rect(area);
        let mut size = screen.shrink(6.0).size();
        if size.x / size.y > 16.0 / 9.0 {
            size.x = size.y * 16.0 / 9.0;
        } else {
            size.y = size.x * 9.0 / 16.0;
        }
        let p = ui.painter();
        p.rect_filled(screen.expand(1.0), 16.0, LINE);
        p.rect_filled(screen, 15.0, DEEP);
        let cap_y = screen.bottom() + 22.0;

        if self.s.source == Source::RemotePlay {
            let (title, hint) = match self.selected_console() {
                _ if self.discovery_failed => ("Network unavailable".into(), "nagito could not open a network socket for Remote Play. Check your firewall.".into()),
                None => ("Looking for your PS5".into(), "Turn on the console and make sure this PC is on the same network. Remote Play must be on: Settings, System, Remote Play.".into()),
                Some(c) => {
                    let hint = match (&c.linked, &c.power) {
                        (None, _) => "Link it once with the code from your PS5, then play any time.".to_string(),
                        (_, Some(remote::Power::Resting)) => "It is in rest mode. Wake it up from here.".into(),
                        (_, Some(remote::Power::Ready)) if !c.app.is_empty() => format!("Playing {}. Press Enter to jump in.", c.app),
                        (_, Some(remote::Power::Ready)) => "Ready. Press Enter to start streaming.".into(),
                        (Some(l), None) if l.shared => "A friend shared this PS5. Press Enter to connect. If it fails, the console may be off or its router ports are not open.".into(),
                        _ => "Not found on your network right now. Turn it on, or check that it is on the same network.".into(),
                    };
                    (c.name, hint)
                }
            };
            Self::stage_message(p, screen, &title, &hint);
            if let Some(c) = self.selected_console() {
                p.text(pos2(screen.left() + 4.0, cap_y), Align2::LEFT_CENTER, &c.addr, FontId::proportional(16.0), PAPER);
                let q = self.s.quality();
                let codec = if q.hevc { "HEVC" } else { "H.264" };
                p.text(pos2(screen.right() - 4.0, cap_y), Align2::RIGHT_CENTER, format!("Streams at {}p {} fps, {codec}, {} Mbps", q.height, q.fps, self.s.bitrate_mbps), FontId::proportional(15.0), HAZE);
            }
            return;
        }

        let stats = self.stats();
        match (&self.live, &stats) {
            (Some(live), Some(s)) if s.width > 0 && s.error.is_none() => {
                let ppp = ui.ctx().pixels_per_point();
                let video_rect = Rect::from_center_size(screen.center(), size);
                p.add(egui_wgpu::Callback::new_paint_callback(
                    video_rect,
                    video::VideoPaint { slot: live.slot.clone(), size_px: [(video_rect.width() * ppp) as u32, (video_rect.height() * ppp) as u32], look: self.look() },
                ));
            }
            _ => {
                let (title, hint) = match &stats {
                    None => ("No capture card", "Plug in your capture card, then choose Scan for devices.".to_string()),
                    Some(s) if s.error.is_some() => ("Capture card stopped", s.error.clone().unwrap_or_default()),
                    _ => ("No picture yet", "Turn on your PS5 and check the HDMI cable going into the capture card.".into()),
                };
                Self::stage_message(p, screen, title, &hint);
            }
        }
        if let Some(v) = &self.video {
            p.text(pos2(screen.left() + 4.0, cap_y), Align2::LEFT_CENTER, &v.name, FontId::proportional(16.0), PAPER);
        }
        if let Some(s) = stats.filter(|s| s.width > 0) {
            let sound = match (&self.s.audio, &s.audio) {
                (_, Some(err)) => err.clone(),
                (Some(_), None) => "Sound on".into(),
                (None, None) => "Sound off".into(),
            };
            p.text(pos2(screen.right() - 4.0, cap_y), Align2::RIGHT_CENTER, sound, FontId::proportional(15.0), HAZE);
        }
    }

    /// Linking walks through the PS5's Link Device screen, so the steps are numbered.
    fn link_stage(&mut self, ui: &mut egui::Ui, area: Rect) {
        let screen = Self::screen_rect(area);
        ui.painter().rect_filled(screen.expand(1.0), 16.0, LINE);
        ui.painter().rect_filled(screen, 15.0, DEEP);
        let col = Rect::from_center_size(screen.center(), vec2(screen.width().min(500.0) - 48.0, screen.height() - 48.0));
        let account_ok = remote::parse_account_id(&self.s.account_id).is_some();
        let mut close = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(col), |ui| {
            let Some(form) = self.link.as_mut() else { return };
            ui.add_space((col.height() - 380.0).max(0.0) / 2.0);
            ui.label(egui::RichText::new(format!("Link {}", form.console.name)).font(display(40.0)).color(PAPER));
            ui.add_space(8.0);
            for (n, step) in [
                "On the PS5, open Settings, then System, then Remote Play, then Link Device.",
                "Type the 8-digit code it shows below.",
                "Make sure your PSN account ID is filled in on the right.",
            ]
            .iter()
            .enumerate()
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(format!("{}", n + 1)).font(display(20.0)).color(LAMP));
                    ui.label(egui::RichText::new(*step).color(PAPER));
                });
            }
            ui.add_space(12.0);
            let busy = form.running.is_some();
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut form.pin).hint_text("8-digit code").char_limit(8).font(display(28.0)).desired_width(ui.available_width()),
            );
            form.pin.retain(|c| c.is_ascii_digit());
            ui.add_space(12.0);

            if let Some(link) = &form.running {
                match link.result() {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Linking with the console…");
                        });
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                    Some(Ok((regist_key, rp_key, nickname))) => {
                        let linked = remote::Linked {
                            name: if nickname.is_empty() { form.console.name.clone() } else { nickname },
                            host_id: form.console.id.clone(),
                            addr: form.console.addr.clone(),
                            ps5: form.console.ps5,
                            regist_key,
                            rp_key,
                            account_id: None,
                            shared: false,
                        };
                        self.s.consoles.retain(|l| l.host_id != linked.host_id);
                        self.s.console = Some(linked.host_id.clone());
                        self.s.consoles.push(linked);
                        close = true;
                    }
                    Some(Err(e)) => {
                        form.error = Some(e);
                        form.running = None;
                    }
                }
            } else {
                if let Some(e) = &form.error {
                    ui.label(egui::RichText::new(e).color(FLARE));
                    ui.add_space(6.0);
                }
                let ready = form.pin.len() == 8 && account_ok;
                ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    let w = ui.available_width() * 0.6;
                    ui.allocate_ui(vec2(w, 60.0), |ui| {
                        if widgets::start_button(ui, ready, "Link console").clicked() {
                            let account = remote::parse_account_id(&self.s.account_id).unwrap_or_default();
                            match remote::Link::start(&form.console.addr, form.console.ps5, account, form.pin.parse().unwrap_or(0)) {
                                Some(l) => {
                                    form.running = Some(l);
                                    form.error = None;
                                }
                                None => form.error = Some("Could not reach the console.".into()),
                            }
                        }
                    });
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                if !account_ok {
                    ui.label(egui::RichText::new("Your PSN account ID is missing or not valid.").size(14.0).color(FLARE));
                }
            }
        });
        if close {
            if self.link.as_ref().is_some_and(|f| f.running.is_some() && self.s.console.as_ref() == Some(&f.console.id)) {
                self.notify("Console linked. You can play now.", true);
            }
            self.link = None;
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, side: Rect, ctx: &egui::Context) {
        ui.painter().rect_filled(side, 20.0, PANEL.gamma_multiply(0.85));
        let content = side.shrink(22.0);
        let start_rect = Rect::from_min_max(pos2(content.left(), content.bottom() - 60.0), content.max);
        let top = Rect::from_min_max(content.min, pos2(content.right(), start_rect.top() - 16.0));

        ui.scope_builder(egui::UiBuilder::new().max_rect(top), |ui| {
            egui::ScrollArea::vertical().id_salt("sidebar").auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                section(ui, "Source");
                let card = self.video.as_ref().map_or("None connected".to_owned(), |v| v.name.clone());
                if widgets::source_row(ui, self.s.source == Source::Capture, "Capture card", &card, true).clicked() {
                    self.s.source = Source::Capture;
                    self.link = None;
                }
                let ps = self.selected_console().map_or("Remote Play over your network".to_owned(), |c| format!("Remote Play to {}", c.name));
                if widgets::source_row(ui, self.s.source == Source::RemotePlay, "PlayStation 5", &ps, true).clicked() {
                    self.s.source = Source::RemotePlay;
                }

                match self.s.source {
                    Source::Capture => self.capture_settings(ui),
                    Source::RemotePlay => self.remote_settings(ui),
                }

                section(ui, "Picture");
                widgets::segmented(ui, &mut self.s.mode, &[(Mode::Responsive, "Responsive"), (Mode::Smooth, "Frame gen")]);
                ui.label(egui::RichText::new(self.mode_note()).size(14.0).color(HAZE));
                ui.add_space(4.0);
                section(ui, "Scaling");
                widgets::segmented(ui, &mut self.s.scaler, &[(Scaler::Fsr, "FSR 1"), (Scaler::Bicubic, "Bicubic"), (Scaler::Bilinear, "Basic")]);
                ui.horizontal(|ui| {
                    ui.label("Sharpen");
                    ui.spacing_mut().slider_width = ui.available_width() - 56.0;
                    ui.add(egui::Slider::new(&mut self.s.sharpness, 0.0..=1.0).show_value(false));
                    ui.label(egui::RichText::new(format!("{:.0}%", self.s.sharpness * 100.0)).color(HAZE));
                });
            });
        });

        let (action, label) = self.action();
        let enabled = !matches!(action, Action::Nothing) && self.link.is_none();
        ui.scope_builder(egui::UiBuilder::new().max_rect(start_rect), |ui| {
            if widgets::start_button(ui, enabled, label).clicked() {
                self.run_action(ctx);
            }
        });
    }

    fn capture_settings(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        let before = self.video.clone();
        egui::ComboBox::from_id_salt("video")
            .width(w)
            .selected_text(self.video.as_ref().map_or("No video device", |v| v.name.as_str()).to_owned())
            .show_ui(ui, |ui| {
                for v in &self.videos {
                    ui.selectable_value(&mut self.video, Some(v.clone()), &v.name);
                }
            });
        if self.video != before {
            self.s.video = self.video.as_ref().map(|v| v.name.clone());
        }
        let modes = match self.live.as_ref().map(|l| &l.session) {
            Some(Session::Capture(c)) => c.modes.lock().unwrap().clone(),
            _ => vec![],
        };
        if !modes.is_empty() {
            let name = |m: Option<capture::CamMode>| m.map_or("Best picture the card offers".to_owned(), |(w, h, f)| format!("{w}×{h} at {f} fps"));
            egui::ComboBox::from_id_salt("capmode").width(w).selected_text(name(self.s.capture_mode)).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.s.capture_mode, None, name(None));
                for m in modes {
                    ui.selectable_value(&mut self.s.capture_mode, Some(m), name(Some(m)));
                }
            });
        }
        egui::ComboBox::from_id_salt("audio")
            .width(w)
            .selected_text(self.s.audio.clone().map_or("Sound off".to_owned(), |a| format!("Sound from {a}")))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.s.audio, None, "Sound off");
                for a in &self.audios {
                    ui.selectable_value(&mut self.s.audio, Some(a.clone()), a);
                }
            });
        if ui.link(egui::RichText::new("Scan for devices").color(LAMP)).clicked() {
            self.rescan();
        }
    }

    fn remote_settings(&mut self, ui: &mut egui::Ui) {
        section(ui, "Consoles");
        let consoles = self.consoles();
        if consoles.is_empty() {
            ui.label(egui::RichText::new("Searching your network…").color(HAZE));
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        let selected = self.selected_console().map(|c| c.id);
        for c in consoles {
            let state = match (&c.linked, &c.power) {
                (None, _) => "Not linked yet",
                (_, Some(remote::Power::Ready)) => "Ready to play",
                (_, Some(remote::Power::Resting)) => "Resting",
                (Some(l), None) if l.shared => "Shared by a friend",
                _ => "Not on your network",
            };
            if widgets::source_row(ui, selected.as_ref() == Some(&c.id), &c.name, state, c.power.is_some()).clicked() {
                self.s.console = Some(c.id.clone());
                self.link = None;
            }
        }
        section(ui, "Friend's PS5");
        ui.add(egui::TextEdit::singleline(&mut self.invite_entry).hint_text("Paste an invite link").desired_width(ui.available_width()));
        if !self.invite_entry.trim().is_empty() {
            match remote::parse_invite(&self.invite_entry) {
                Some(l) => {
                    let name = l.name.clone();
                    self.s.consoles.retain(|c| c.host_id != l.host_id);
                    self.s.console = Some(l.host_id.clone());
                    self.s.consoles.push(l);
                    self.invite_entry.clear();
                    self.notify(format!("Added {name}. Over the internet, a bitrate of 10 to 20 Mbps works best."), true);
                }
                None => {
                    ui.label(egui::RichText::new("That is not a nagito invite link.").size(14.0).color(FLARE));
                }
            }
        }
        section(ui, "PSN account ID");
        let valid = remote::parse_account_id(&self.s.account_id).is_some();
        ui.add(egui::TextEdit::singleline(&mut self.s.account_id).hint_text("For example AbCdEfGh123=").desired_width(ui.available_width()));
        if let Some(job) = &self.psn_job {
            let done = job.lock().unwrap().take();
            match done {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Getting your account ID from PSN…").size(14.0).color(HAZE));
                    });
                    ui.ctx().request_repaint_after(Duration::from_millis(150));
                }
                Some(Ok(id)) => {
                    self.s.account_id = id;
                    self.psn_job = None;
                    self.psn_redirect.clear();
                }
                Some(Err(e)) => {
                    self.psn_job = None;
                    self.psn_redirect.clear(); // otherwise the same bad code retries every frame
                    self.notify(e, false);
                }
            }
        } else if !valid {
            ui.label(egui::RichText::new("Needed to link and stream. Sign in to PSN in your browser, then paste the address the page ends on.").size(14.0).color(HAZE));
            if ui.button("Sign in to PSN").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(psn::login_url()));
            }
            ui.add(egui::TextEdit::singleline(&mut self.psn_redirect).hint_text("Paste the address here").desired_width(ui.available_width()));
            if let Some(code) = psn::code_from_redirect(&self.psn_redirect) {
                let job = Arc::new(std::sync::Mutex::new(None));
                let (j, c) = (job.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    *j.lock().unwrap() = Some(psn::fetch_account_id(&code));
                    c.request_repaint();
                });
                self.psn_job = Some(job);
            }
        }
    }

    /// Everything that is set once and rarely touched, so the sidebar stays short.
    fn settings_stage(&mut self, ui: &mut egui::Ui, area: Rect) {
        let screen = Self::screen_rect(area);
        ui.painter().rect_filled(screen, 20.0, PANEL.gamma_multiply(0.85));
        let col = Rect::from_min_max(screen.min + vec2(32.0, 28.0), screen.max - vec2(32.0, 20.0));
        let mut forget = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(col), |ui| {
            egui::ScrollArea::vertical().id_salt("settings").auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                ui.label(egui::RichText::new("Settings").font(display(40.0)).color(PAPER));
                let note = |ui: &mut egui::Ui, t: &str| {
                    ui.label(egui::RichText::new(t).size(14.0).color(HAZE));
                };
                ui.columns(2, |cols| {
                let (left, right) = cols.split_at_mut(1);
                let (ui, ui2) = (&mut left[0], &mut right[0]);
                ui.spacing_mut().item_spacing.y = 8.0;
                ui2.spacing_mut().item_spacing.y = 8.0;
                ui.set_max_width(ui.available_width() - 24.0);

                group(ui, "Remote Play stream");
                section(ui, "Resolution");
                widgets::segmented(ui, &mut self.s.res, &[(Res::P1080, "1080p"), (Res::P720, "720p")]);
                section(ui, "Frame rate");
                widgets::segmented(ui, &mut self.s.fps, &[(60, "60 fps"), (30, "30 fps")]);
                note(ui, "GTA VI runs at 30 fps on PS5. Streaming at 30 spends the whole bitrate on half as many frames, so each one is sharper. Pair it with Smooth 60.");
                section(ui, "Video codec");
                widgets::segmented(ui, &mut self.s.hevc, &[(true, "HEVC"), (false, "H.264")]);
                note(ui, "HEVC looks better at the same bitrate. Use H.264 only if the picture fails to start.");
                section(ui, "Bitrate");
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = ui.available_width() - 80.0;
                    ui.add(egui::Slider::new(&mut self.s.bitrate_mbps, 10..=100).show_value(false));
                    ui.label(egui::RichText::new(format!("{} Mbps", self.s.bitrate_mbps)).color(HAZE));
                });
                note(ui, "Higher is sharper. 50 to 100 is fine over Ethernet; lower it on Wi-Fi if the picture stutters.");

                group(ui, "Display");
                section(ui, "Frame delivery");
                widgets::segmented(ui, &mut self.s.present, &[(Present::LowDelay, "Low delay"), (Present::Tearing, "No sync"), (Present::Vsync, "VSync")]);
                note(
                    ui,
                    match self.s.present {
                        Present::LowDelay => "Shows the newest frame at each refresh without tearing.",
                        Present::Tearing => "Shows frames the instant they are ready. Best with G-Sync or FreeSync: the screen then refreshes exactly when each frame lands, so 60 fps is perfectly even. Without them it may tear.",
                        Present::Vsync => "Waits for the display. About one refresh more delay.",
                    },
                );
                widgets::toggle(ui, &mut self.s.start_fullscreen, "Play in fullscreen");
                widgets::toggle(ui, &mut self.s.show_stats, "Show stats while playing (F1)");

                let ui = ui2;
                group(ui, "Sound");
                ui.horizontal(|ui| {
                    ui.label("Volume");
                    ui.spacing_mut().slider_width = ui.available_width() - 56.0;
                    ui.add(egui::Slider::new(&mut self.s.volume, 0.0..=1.0).show_value(false));
                    ui.label(egui::RichText::new(format!("{:.0}%", self.s.volume * 100.0)).color(HAZE));
                });

                group(ui, "Controller");
                widgets::toggle(ui, &mut self.s.rumble, "Rumble");
                widgets::toggle(ui, &mut self.s.triggers, "Adaptive triggers on DualSense");
                widgets::toggle(ui, &mut self.s.haptics, "DualSense haptics (USB cable only)");
                widgets::toggle(ui, &mut self.s.motion, "Motion controls");
                widgets::toggle(ui, &mut self.s.touchpad, "Touchpad");
                widgets::toggle(ui, &mut self.s.mic, "Microphone (press the mic button to talk)");
                note(ui, "Controller changes apply the next time you start playing.");
                egui::CollapsingHeader::new("Button layout").id_salt("remap").show(ui, |ui| {
                    for (i, pad) in remote::BUTTONS.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(*pad);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let cur = &mut self.s.button_map[i];
                                egui::ComboBox::from_id_salt(("map", i)).width(150.0).selected_text(remote::BUTTONS[*cur as usize & 15]).show_ui(ui, |ui| {
                                    for (j, ps) in remote::BUTTONS.iter().enumerate() {
                                        ui.selectable_value(cur, j as u8, *ps);
                                    }
                                });
                            });
                        });
                    }
                    if ui.button("Reset layout").clicked() {
                        self.s.button_map = std::array::from_fn(|i| i as u8);
                    }
                });

                group(ui, "Keyboard and mouse");
                widgets::toggle(ui, &mut self.s.kbm, "Play with keyboard and mouse");
                note(ui, "The mouse moves the right stick, left click is R2 and right click is L2. Press Esc to get the cursor back.");
                ui.horizontal(|ui| {
                    ui.label("Mouse speed");
                    ui.spacing_mut().slider_width = ui.available_width() - 56.0;
                    ui.add(egui::Slider::new(&mut self.s.mouse_sens, 0.2..=3.0).show_value(false));
                    ui.label(egui::RichText::new(format!("{:.1}×", self.s.mouse_sens)).color(HAZE));
                });
                egui::CollapsingHeader::new("Key bindings").id_salt("keys").show(ui, |ui| {
                    note(ui, "Click a key, then press the new one. Esc clears it.");
                    for i in 0..KB_DEFAULT.len() {
                        ui.horizontal(|ui| {
                            ui.label(kb_action_name(i));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let text = if self.rebinding == Some(i) {
                                    "Press a key…".to_owned()
                                } else if self.s.keys[i].is_empty() {
                                    "None".to_owned()
                                } else {
                                    self.s.keys[i].clone()
                                };
                                if ui.add(egui::Button::new(text).min_size(vec2(150.0, 0.0))).clicked() {
                                    self.rebinding = Some(i);
                                }
                            });
                        });
                    }
                    if ui.button("Reset keys").clicked() {
                        self.s.keys = KB_DEFAULT.iter().map(|k| k.to_string()).collect();
                    }
                });
                if let Some(i) = self.rebinding {
                    let pressed = ui.ctx().input(|inp| {
                        inp.events.iter().find_map(|e| match e {
                            egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                            _ => None,
                        })
                    });
                    if let Some(k) = pressed {
                        self.s.keys[i] = if k == egui::Key::Escape { String::new() } else { k.name().to_owned() };
                        self.rebinding = None;
                    }
                }

                group(ui, "Linked consoles");
                if self.s.consoles.is_empty() {
                    note(ui, "None yet. Link a PS5 from the PlayStation 5 source.");
                }
                let mut invite = None;
                for c in &self.s.consoles {
                    ui.horizontal(|ui| {
                        ui.label(&c.name);
                        ui.label(egui::RichText::new(if c.shared { "from a friend" } else { &c.addr }).color(HAZE));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Forget").clicked() {
                                forget = Some(c.host_id.clone());
                            }
                            if !c.shared && ui.button("Invite a friend").clicked() {
                                invite = Some(c.clone());
                            }
                        });
                    });
                }
                if let Some(console) = invite {
                    let job = Arc::new(std::sync::Mutex::new(None));
                    let (j, ctx, ip) = (job.clone(), ui.ctx().clone(), console.addr.clone());
                    std::thread::spawn(move || {
                        let (n, mut ext) = remote::upnp_forward(&ip);
                        if ext.is_empty() {
                            // the router would not say; ask a public what-is-my-IP service
                            ext = ureq::get("https://api.ipify.org").call().ok().and_then(|mut r| r.body_mut().read_to_string().ok()).unwrap_or_default();
                        }
                        *j.lock().unwrap() = Some((n, ext.trim().to_owned()));
                        ctx.request_repaint();
                    });
                    self.share = Some(Share { console, job, ports: None, addr: String::new() });
                }
                self.share_panel(ui);
                note(ui, "Links are saved, so you enter the PS5 code only once per console.");
                });
            });
        });
        if let Some(id) = forget {
            self.s.consoles.retain(|c| c.host_id != id);
        }
    }

    fn player(&mut self, ui: &mut egui::Ui, avail: Rect) {
        let Some(live) = &self.live else { return };
        let stats = live.slot.stats.lock().unwrap().clone();
        let aspect = if stats.height > 0 { stats.width as f32 / stats.height as f32 } else { 16.0 / 9.0 };
        let mut size = avail.size();
        if size.x / size.y > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }
        let rect = Rect::from_center_size(avail.center(), size);
        let ppp = ui.ctx().pixels_per_point();
        let p = ui.painter();
        p.rect_filled(avail, 0.0, Color32::BLACK);
        if stats.width > 0 {
            p.add(egui_wgpu::Callback::new_paint_callback(
                rect,
                video::VideoPaint { slot: live.slot.clone(), size_px: [(size.x * ppp).round() as u32, (size.y * ppp).round() as u32], look: self.look() },
            ));
        } else if stats.error.is_none() {
            p.text(avail.center(), Align2::CENTER_CENTER, "Connecting…", display(36.0), PAPER);
        }
        if let Some(e) = &stats.error {
            p.text(avail.center(), Align2::CENTER_CENTER, e, FontId::proportional(18.0), FLARE);
        }
        if live.key.is_none() && stats.controller.as_deref().is_none_or(str::is_empty) {
            let r = Rect::from_center_size(pos2(avail.center().x, avail.bottom() - 48.0), vec2(520.0, 44.0));
            p.rect_filled(r, 12.0, PANEL_HI);
            p.text(r.center(), Align2::CENTER_CENTER, "No controller found. Connect a DualSense by USB cable or Bluetooth.", FontId::proportional(15.0), PAPER);
        }
        if self.look().frame_gen {
            // Wake up again when the real frame should replace the generated one.
            let half = Duration::from_secs_f32(0.5 / stats.source_fps.clamp(20.0, 240.0));
            let real_at = *live.slot.real_at.lock().unwrap();
            if live.slot.has_pending() {
                ui.ctx().request_repaint_after(half);
            } else if let Some(t) = real_at {
                ui.ctx().request_repaint_after(t.saturating_duration_since(Instant::now()));
            }
        }
        if self.s.show_stats {
            hud(ui, avail.left_top() + vec2(16.0, 16.0), &stats);
        }
        if let Some(wrong) = stats.pin {
            self.pin_prompt(ui, avail, wrong);
        }
    }

    fn share_panel(&mut self, ui: &mut egui::Ui) {
        let Some(share) = self.share.as_mut() else { return };
        let note = |ui: &mut egui::Ui, t: &str, c: Color32| {
            ui.label(egui::RichText::new(t).size(14.0).color(c));
        };
        ui.add_space(8.0);
        ui.label(egui::RichText::new(format!("Invite a friend to {}", share.console.name)).font(display(22.0)).color(PAPER));
        if share.ports.is_none() {
            if let Some((n, ext)) = share.job.lock().unwrap().take() {
                share.ports = Some(n);
                share.addr = ext;
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Opening your router for the PS5…");
                });
                ui.ctx().request_repaint_after(Duration::from_millis(150));
                return;
            }
        }
        let manual = format!("In your router settings, forward TCP 9295 and UDP 9295, 9296, 9297 and 9302 to {}.", share.console.addr);
        match share.ports {
            Some(5) => note(ui, "Router ports are open for your PS5.", SIGNAL),
            Some(n) if n > 0 => note(ui, &format!("Your router opened only {n} of 5 ports. {manual}"), FLARE),
            _ => note(ui, &format!("Your router did not let nagito open ports. {manual}"), FLARE),
        }
        ui.horizontal(|ui| {
            ui.label("Your address");
            ui.add(egui::TextEdit::singleline(&mut share.addr).hint_text("Public IP or hostname").desired_width(ui.available_width()));
        });
        let ready = !share.addr.trim().is_empty() && remote::parse_account_id(&self.s.account_id).is_some();
        let mut close = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new("Copy invite link")).clicked() {
                ui.ctx().copy_text(remote::invite(&share.console, &share.addr, &self.s.account_id));
                self.notice = Some(("Invite copied. Send it to your friend; they paste it into nagito.".into(), true, Instant::now()));
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if !ready {
            note(ui, "Needs your public address and your PSN account ID.", FLARE);
        }
        note(
            ui,
            "The link holds your PS5's Remote Play keys and your PSN account ID, so only send it to someone you trust. They can play whenever the PS5 is on, even when nagito is closed. To cut them off, remove this PC under Settings, System, Remote Play, Linked devices on the PS5, then link again.",
            HAZE,
        );
        if close {
            self.share = None;
        }
    }

    /// The PS5 asks for its login passcode when the user profile has one.
    fn pin_prompt(&mut self, ui: &mut egui::Ui, avail: Rect, wrong: bool) {
        let r = Rect::from_center_size(avail.center(), vec2(420.0, 220.0));
        ui.painter().rect_filled(r, 16.0, PANEL);
        let mut send = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(24.0)), |ui| {
            ui.label(egui::RichText::new("PS5 passcode").font(display(30.0)).color(PAPER));
            let hint = if wrong { "That passcode was wrong. Try again." } else { "Your PS5 profile is locked. Type its 4-digit passcode." };
            ui.label(egui::RichText::new(hint).color(if wrong { FLARE } else { HAZE }));
            let edit = ui.add(egui::TextEdit::singleline(&mut self.pin_entry).password(true).char_limit(4).font(display(28.0)).desired_width(ui.available_width()));
            edit.request_focus();
            self.pin_entry.retain(|c| c.is_ascii_digit());
            send = self.pin_entry.len() == 4 && (ui.button("Unlock").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)));
        });
        if send {
            if let Some(Session::Remote(stream)) = self.live.as_ref().map(|l| &l.session) {
                stream.set_pin(&self.pin_entry);
            }
            self.pin_entry.clear();
        }
    }

    /// Keyboard and mouse as a controller: keys from the bindings, mouse speed on the right stick.
    fn kbm_input(&mut self, ctx: &egui::Context) -> remote::Input {
        let s = &self.s;
        let (input, motion, dt) = ctx.input(|i| {
            let down = |a: usize| s.key(a).is_some_and(|k| i.key_down(k));
            let mut inp = remote::Input::default();
            for b in 0..16 {
                if down(b) {
                    inp.buttons |= 1 << b;
                }
            }
            inp.l2 = if down(16) || i.pointer.secondary_down() { 255 } else { 0 };
            inp.r2 = if down(17) || i.pointer.primary_down() { 255 } else { 0 };
            let axis = |neg: usize, pos: usize| (down(pos) as i32 - down(neg) as i32) * 32767;
            inp.ly = axis(18, 19) as i16;
            inp.lx = axis(20, 21) as i16;
            let motion: egui::Vec2 = i.events.iter().filter_map(|e| if let egui::Event::MouseMoved(d) = e { Some(*d) } else { None }).fold(egui::Vec2::ZERO, |a, b| a + b);
            (inp, motion, i.unstable_dt.max(1.0 / 500.0))
        });
        // Mouse speed in px/s becomes stick deflection, pushed past the game's dead zone.
        let target = (motion / dt * (s.mouse_sens / 1800.0)).clamp(egui::Vec2::splat(-1.0), egui::Vec2::splat(1.0));
        self.mouse_stick = self.mouse_stick * 0.4 + target * 0.6;
        let stick = |v: f32| if v.abs() < 0.01 { 0 } else { ((v.signum() * (0.2 + 0.8 * v.abs())) * 32767.0) as i16 };
        remote::Input { rx: stick(self.mouse_stick.x), ry: stick(self.mouse_stick.y), ..input }
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(title).size(15.0).color(HAZE));
}

/// A settings group heading, one step up from `section`.
fn group(ui: &mut egui::Ui, title: &str) {
    ui.add_space(18.0);
    ui.label(egui::RichText::new(title).font(display(24.0)).color(LAMP));
}

fn hud(ui: &egui::Ui, at: egui::Pos2, s: &video::Stats) {
    let age_color = if s.frame_age_ms <= 20.0 { SIGNAL } else { FLARE };
    let mut rows: Vec<(&str, String, Color32)> = vec![
        ("Source", format!("{}×{} at {:.0} fps", s.width, s.height, s.source_fps), PAPER),
        ("Frame age", format!("{:.1} ms", s.frame_age_ms), age_color),
        ("Shown", format!("{:.0} fps", s.shown_fps), PAPER),
        ("Dropped", s.dropped.to_string(), PAPER),
    ];
    if s.decode_ms > 0.0 {
        rows.insert(1, ("Decode", format!("{:.1} ms", s.decode_ms), PAPER));
    }
    if s.rtt_ms > 0.0 {
        rows.push(("Network delay", format!("{:.1} ms", s.rtt_ms), if s.rtt_ms <= 10.0 { SIGNAL } else { PAPER }));
        rows.push(("Packet loss", format!("{:.2}%", s.loss * 100.0), if s.loss < 0.005 { PAPER } else { FLARE }));
        rows.push(("Bitrate", format!("{:.1} Mbps", s.bitrate_mbps), PAPER));
    }
    if let Some(muted) = s.mic {
        rows.push(("Microphone", if muted { "Muted".into() } else { "On".into() }, if muted { HAZE } else { SIGNAL }));
    }
    if let Some(c) = &s.controller {
        rows.push(("Controller", if c.is_empty() { "None".into() } else { c.clone() }, if c.is_empty() { FLARE } else { PAPER }));
    }
    let p = ui.painter();
    let bg = Rect::from_min_size(at, vec2(280.0, 24.0 * rows.len() as f32 + 16.0));
    p.rect_filled(bg, 12.0, Color32::from_rgba_unmultiplied(23, 22, 42, 215));
    for (i, (k, v, c)) in rows.iter().enumerate() {
        let y = at.y + 8.0 + 24.0 * i as f32;
        p.text(pos2(at.x + 14.0, y), Align2::LEFT_TOP, *k, FontId::proportional(14.0), HAZE);
        p.text(pos2(bg.right() - 14.0, y), Align2::RIGHT_TOP, v, FontId::proportional(15.0), *c);
    }
    if let Some(a) = &s.audio {
        p.text(pos2(at.x, bg.bottom() + 8.0), Align2::LEFT_TOP, a, FontId::proportional(14.0), FLARE);
    }
}

impl eframe::App for App {
    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(5) // a fresh console link must survive a crash or a killed process
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "nagito", &self.s);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        // Smooth 60 no longer forces VSync: on a high-refresh screen that made 60 fps judder (2-3 refresh cadence).
        let present_mode = match self.s.present {
            Present::LowDelay => wgpu::PresentMode::Mailbox,
            Present::Tearing => wgpu::PresentMode::Immediate, // lets G-Sync/FreeSync refresh exactly on each frame
            Present::Vsync => wgpu::PresentMode::Fifo,
        };
        frame.set_wgpu_surface_config(egui_wgpu::SurfaceConfig { present_mode, desired_maximum_frame_latency: Some(1) });
        capture::set_volume(self.s.volume);
        // Dev screenshots never open a camera; Remote Play discovery is just a network broadcast.
        if std::env::var_os("NAGITO_SCREENSHOT").is_none() || self.s.source == Source::RemotePlay {
            self.sync_live(ctx);
        }
        if self.playing && self.live.is_none() {
            self.play(ctx, false);
        }
        // A Remote Play session that ended returns to the launcher with the reason.
        if self.playing && self.s.source == Source::RemotePlay {
            if let Some(e) = self.stats().and_then(|s| s.error) {
                self.play(ctx, false);
                self.live = None;
                self.notify(e, false);
            }
        }

        let (esc, f1, f11) = ctx.input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::F1), i.key_pressed(egui::Key::F11)));
        if f11 {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }
        if self.playing {
            if esc {
                self.play(ctx, false);
            }
            if f1 {
                self.s.show_stats = !self.s.show_stats;
            }
        }
        let pin_open = self.stats().is_some_and(|s| s.pin.is_some());
        let kbm = self.playing && self.s.kbm && !pin_open;
        let input = if kbm { self.kbm_input(ctx) } else { remote::Input::default() };
        if let Some(Session::Remote(stream)) = self.live.as_ref().map(|l| &l.session) {
            stream.poll_stats();
            stream.set_input(input);
        }
        let grab = kbm && self.s.source == Source::RemotePlay;
        if grab != self.grabbed {
            self.grabbed = grab;
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(if grab { egui::CursorGrab::Confined } else { egui::CursorGrab::None }));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!grab));
        }

        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
            let full = ui.max_rect();
            if self.playing {
                self.player(ui, full);
            } else {
                widgets::backdrop(ui.painter(), full);
                self.launcher(ui, full);
            }
        });
        if let Ok(path) = std::env::var("NAGITO_SCREENSHOT") {
            dev_screenshot(ctx, &path);
        }
    }
}

/// Dev aid: render for a few seconds, save the window as a BMP, then exit. Never steals focus.
fn dev_screenshot(ctx: &egui::Context, path: &str) {
    ctx.request_repaint();
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ctx.input(|i| i.time) < 4.0 || TAKEN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let (c, path) = (ctx.clone(), path.to_owned());
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
        let [w, h] = img.size;
        let mut bmp = Vec::with_capacity(54 + w * h * 4);
        bmp.extend_from_slice(b"BM");
        for v in [(54 + w * h * 4) as u32, 0, 54, 40, w as u32, (h as i32).wrapping_neg() as u32] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        bmp.extend_from_slice(&[1, 0, 32, 0]);
        bmp.extend_from_slice(&[0; 24]);
        for p in &img.pixels {
            bmp.extend_from_slice(&[p.b(), p.g(), p.r(), 255]);
        }
        let _ = std::fs::write(path, bmp);
        c.send_viewport_cmd(egui::ViewportCommand::Close);
        c.request_repaint();
    })));
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(ms: u32) -> u32;
}

fn main() -> eframe::Result {
    // 1 ms timer resolution so generated and real frames are presented on time, not on 15.6 ms ticks.
    unsafe { timeBeginPeriod(1) };
    // There is no console window, so panics go to %APPDATA%\nagito\crash.log.
    std::panic::set_hook(Box::new(|info| {
        let dir = std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("nagito");
        let _ = std::fs::create_dir_all(&dir);
        let msg = format!("{info}\n{}\n", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log")).and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
    }));
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        // DX12 so AMD Fluid Motion Frames and NVIDIA Smooth Motion can hook our swapchain.
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        setup.power_preference = wgpu::PowerPreference::HighPerformance;
    }
    let screenshot = std::env::var_os("NAGITO_SCREENSHOT").is_some();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("nagito")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1000.0, 640.0])
            .with_active(!screenshot),
        persist_window: !screenshot,
        ..Default::default()
    };
    eframe::run_native("nagito", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
