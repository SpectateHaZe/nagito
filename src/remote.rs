//! PS5 Remote Play through the native chiaki-ng shim (native/nagito_rp.c).

use crate::capture::{output_stream, push_audio, AudioQueue};
use crate::video::{Frame, FrameSlot, Pixels};
use cpal::traits::StreamTrait;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[repr(C)]
struct RawHost {
    addr: [c_char; 64],
    name: [c_char; 64],
    id: [c_char; 32],
    app: [c_char; 64],
    state: i32,
    ps5: i32,
}

#[repr(C)]
struct RawRegistered {
    nickname: [c_char; 32],
    mac: [u8; 6],
    regist_key: [u8; 16],
    rp_key: [u8; 16],
}

#[repr(C)]
struct RawStreamConfig {
    host: *const c_char,
    ps5: i32,
    regist_key: [u8; 16],
    rp_key: [u8; 16],
    account_id: [u8; 8],
    width: i32,
    height: i32,
    fps: i32,
    bitrate_kbps: i32,
    hevc: i32,
    rumble: i32,
    triggers: i32,
    motion: i32,
    touchpad: i32,
    haptics: i32,
    mic: i32,
    button_map: [u32; 16],
}

/// Keyboard and mouse state, merged with the controllers by the shim.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Input {
    pub buttons: u32,
    pub l2: u8,
    pub r2: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

#[repr(C)]
#[derive(Default)]
struct RawStats {
    rtt_ms: f32,
    loss: f32,
    bitrate_mbps: f32,
    decode_ms: f32,
}

#[repr(C)]
struct RawCallbacks {
    user: *mut c_void,
    video: extern "C" fn(*mut c_void, *const u8, i32, i32, i32),
    audio: extern "C" fn(*mut c_void, *const i16, usize, i32, i32),
    event: extern "C" fn(*mut c_void, i32, i32, *const c_char),
}

type HostsCb = extern "C" fn(*mut c_void, *const RawHost, i32);
type RegistCb = extern "C" fn(*mut c_void, i32, *const RawRegistered);

#[link(name = "nagito_rp")]
unsafe extern "C" {
    fn nagito_init() -> i32;
    fn nagito_discovery_start(cb: HostsCb, user: *mut c_void) -> *mut c_void;
    fn nagito_discovery_stop(d: *mut c_void);
    fn nagito_wakeup(host: *const c_char, regist_key: *const u8, ps5: i32) -> i32;
    fn nagito_regist_start(host: *const c_char, ps5: i32, account_id: *const u8, pin: u32, cb: RegistCb, user: *mut c_void) -> *mut c_void;
    fn nagito_regist_free(r: *mut c_void);
    fn nagito_stream_start(cfg: *const RawStreamConfig, cb: *const RawCallbacks, err: *mut c_char, err_size: usize) -> *mut c_void;
    fn nagito_stream_stop(s: *mut c_void);
    fn nagito_stream_set_pin(s: *mut c_void, pin: *const c_char);
    fn nagito_stream_set_input(s: *mut c_void, input: *const Input);
    fn nagito_stream_stats(s: *mut c_void, out: *mut RawStats);
}

/// PS5 buttons in the shim's order; bit i of a button mask is entry i.
pub const BUTTONS: [&str; 16] = [
    "Cross", "Circle", "Square", "Triangle", "D-pad left", "D-pad right", "D-pad up", "D-pad down", "L1", "R1", "L3", "R3", "Options", "Create", "Touchpad", "PS",
];

fn text(raw: &[c_char]) -> String {
    let bytes: Vec<u8> = raw.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn init() -> bool {
    static INIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *INIT.get_or_init(|| unsafe { nagito_init() } == 0)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Power {
    Ready,
    Resting,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Host {
    pub addr: String,
    pub name: String,
    pub id: String,
    pub app: String,
    pub power: Power,
    pub ps5: bool,
}

/// A console this PC is linked to. Keys come from registration and are needed to stream or wake it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Linked {
    pub name: String,
    pub host_id: String,
    pub addr: String,
    pub ps5: bool,
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
}

/// Decodes the base64 PSN account ID the console needs for linking and streaming.
pub fn parse_account_id(b64: &str) -> Option<[u8; 8]> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?.try_into().ok()
}

// ---------- discovery ----------

struct DiscoveryState {
    hosts: Mutex<Vec<Host>>,
    repaint: Box<dyn Fn() + Send + Sync>,
}

pub struct Discovery {
    raw: *mut c_void,
    state: Arc<DiscoveryState>,
}

extern "C" fn on_hosts(user: *mut c_void, hosts: *const RawHost, count: i32) {
    let state = unsafe { &*(user as *const DiscoveryState) };
    let raw = unsafe { std::slice::from_raw_parts(hosts, count.max(0) as usize) };
    let list = raw
        .iter()
        .map(|h| Host {
            addr: text(&h.addr),
            name: text(&h.name),
            id: text(&h.id),
            app: text(&h.app),
            power: match h.state {
                1 => Power::Ready,
                2 => Power::Resting,
                _ => Power::Unknown,
            },
            ps5: h.ps5 != 0,
        })
        .collect();
    *state.hosts.lock().unwrap() = list;
    (state.repaint)();
}

impl Discovery {
    pub fn start(repaint: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        if !init() {
            return None;
        }
        let state = Arc::new(DiscoveryState { hosts: Mutex::new(vec![]), repaint: Box::new(repaint) });
        let raw = unsafe { nagito_discovery_start(on_hosts, Arc::as_ptr(&state) as *mut c_void) };
        (!raw.is_null()).then_some(Self { raw, state })
    }

    pub fn hosts(&self) -> Vec<Host> {
        self.state.hosts.lock().unwrap().clone()
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        unsafe { nagito_discovery_stop(self.raw) };
    }
}

pub fn wake(console: &Linked) -> bool {
    let Ok(host) = CString::new(console.addr.clone()) else { return false };
    init() && unsafe { nagito_wakeup(host.as_ptr(), console.regist_key.as_ptr(), console.ps5 as i32) } == 0
}

// ---------- linking ----------

pub type LinkResult = Option<Result<([u8; 16], [u8; 16], String), String>>;

pub struct Link {
    raw: *mut c_void,
    result: Arc<Mutex<LinkResult>>,
}

extern "C" fn on_regist(user: *mut c_void, ok: i32, host: *const RawRegistered) {
    let result = unsafe { &*(user as *const Mutex<LinkResult>) };
    *result.lock().unwrap() = Some(if ok != 0 && !host.is_null() {
        let h = unsafe { &*host };
        Ok((h.regist_key, h.rp_key, text(&h.nickname)))
    } else {
        Err("The console turned down the link. Check the code on the PS5 screen and your account ID, then try again.".into())
    });
}

impl Link {
    pub fn start(addr: &str, ps5: bool, account_id: [u8; 8], pin: u32) -> Option<Self> {
        let host = CString::new(addr).ok()?;
        if !init() {
            return None;
        }
        let result = Arc::new(Mutex::new(None));
        let raw = unsafe {
            nagito_regist_start(host.as_ptr(), ps5 as i32, account_id.as_ptr(), pin, on_regist, Arc::as_ptr(&result) as *mut c_void)
        };
        (!raw.is_null()).then_some(Self { raw, result })
    }

    pub fn result(&self) -> LinkResult {
        self.result.lock().unwrap().clone()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        unsafe { nagito_regist_free(self.raw) };
    }
}

// ---------- streaming ----------

pub struct Quality {
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    pub bitrate_kbps: i32,
    pub hevc: bool,
    pub rumble: bool,
    pub triggers: bool,
    pub motion: bool,
    pub touchpad: bool,
    pub haptics: bool,
    pub mic: bool,
    /// For each pad button (BUTTONS order), the index of the PS5 button it sends.
    pub button_map: [u8; 16],
}

struct StreamState {
    slot: Arc<FrameSlot>,
    audio: AudioQueue,
    repaint: Box<dyn Fn() + Send + Sync>,
    fps_window: Mutex<(u32, Instant)>,
}

pub struct Stream {
    raw: *mut c_void,
    state: Arc<StreamState>,
    audio_stop: Arc<AtomicBool>,
    last_input: Mutex<Input>,
    stats_at: Mutex<Instant>,
}

// PS5 Remote Play audio is 48 kHz stereo.
const AUDIO_RATE: u32 = 48_000;
const AUDIO_CH: usize = 2;

extern "C" fn on_video(user: *mut c_void, nv12: *const u8, w: i32, h: i32, flags: i32) {
    let s = unsafe { &*(user as *const StreamState) };
    let (w, h) = (w as u32, h as u32);
    let data = unsafe { std::slice::from_raw_parts(nv12, (w * h * 3 / 2) as usize) }.to_vec();
    let pixels = Pixels::Nv12 { full_range: flags & 1 != 0, bt601: flags & 2 != 0 };
    s.slot.push(Frame { width: w, height: h, pixels, data, captured_at: Instant::now() });
    {
        let mut win = s.fps_window.lock().unwrap();
        win.0 += 1;
        let mut st = s.slot.stats.lock().unwrap();
        (st.width, st.height) = (w, h);
        if win.1.elapsed() >= Duration::from_secs(1) {
            st.source_fps = win.0 as f32 / win.1.elapsed().as_secs_f32();
            *win = (0, Instant::now());
        }
    }
    (s.repaint)();
}

extern "C" fn on_audio(user: *mut c_void, pcm: *const i16, frames: usize, ch: i32, _rate: i32) {
    let s = unsafe { &*(user as *const StreamState) };
    let pcm = unsafe { std::slice::from_raw_parts(pcm, frames * ch.max(1) as usize) };
    push_audio(&s.audio, pcm.iter().map(|&x| x as f32 / 32768.0), AUDIO_RATE, AUDIO_CH);
}

extern "C" fn on_event(user: *mut c_void, kind: i32, value: i32, msg: *const c_char) {
    let s = unsafe { &*(user as *const StreamState) };
    let msg = unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned();
    let mut st = s.slot.stats.lock().unwrap();
    match kind {
        1 => st.error = Some(if value != 0 { format!("Remote Play ended: {msg}") } else { "The PS5 ended the session.".into() }),
        2 => st.controller = Some(if value > 0 { msg } else { String::new() }),
        3 => st.pin = Some(value != 0),
        4 if !msg.is_empty() => st.audio = Some(format!("Microphone unavailable: {msg}")),
        4 => st.mic = Some(value != 0),
        _ => {}
    }
    drop(st);
    (s.repaint)();
}

impl Stream {
    pub fn start(console: &Linked, account_id: [u8; 8], q: &Quality, slot: Arc<FrameSlot>, repaint: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        if !init() {
            return Err("Remote Play could not start its network layer.".into());
        }
        let host = CString::new(console.addr.clone()).map_err(|_| "Bad console address".to_string())?;
        let audio: AudioQueue = Arc::new(Mutex::new(Default::default()));
        let state = Arc::new(StreamState { slot, audio: audio.clone(), repaint: Box::new(repaint), fps_window: Mutex::new((0, Instant::now())) });
        let cfg = RawStreamConfig {
            host: host.as_ptr(),
            ps5: console.ps5 as i32,
            regist_key: console.regist_key,
            rp_key: console.rp_key,
            account_id,
            width: q.width,
            height: q.height,
            fps: q.fps,
            bitrate_kbps: q.bitrate_kbps,
            hevc: q.hevc as i32,
            rumble: q.rumble as i32,
            triggers: q.triggers as i32,
            motion: q.motion as i32,
            touchpad: q.touchpad as i32,
            haptics: q.haptics as i32,
            mic: q.mic as i32,
            button_map: q.button_map.map(|i| 1u32 << (i & 15)),
        };
        let cbs = RawCallbacks { user: Arc::as_ptr(&state) as *mut c_void, video: on_video, audio: on_audio, event: on_event };
        let mut err = [0 as c_char; 256];
        let raw = unsafe { nagito_stream_start(&cfg, &cbs, err.as_mut_ptr(), err.len()) };
        if raw.is_null() {
            return Err(text(&err));
        }

        // cpal streams are not Send, so the output lives on its own thread.
        let audio_stop = Arc::new(AtomicBool::new(false));
        let (stop, slot) = (audio_stop.clone(), state.slot.clone());
        std::thread::spawn(move || match output_stream(audio, AUDIO_RATE, AUDIO_CH).and_then(|s| Ok(s.play().map(|_| s)?)) {
            Ok(_stream) => {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Err(e) => slot.stats.lock().unwrap().audio = Some(format!("No sound: {e}")),
        });
        Ok(Self { raw, state, audio_stop, last_input: Mutex::new(Input::default()), stats_at: Mutex::new(Instant::now()) })
    }

    /// Answers the PS5's login passcode prompt.
    pub fn set_pin(&self, pin: &str) {
        if let Ok(pin) = CString::new(pin) {
            unsafe { nagito_stream_set_pin(self.raw, pin.as_ptr()) };
            self.state.slot.stats.lock().unwrap().pin = None;
        }
    }

    pub fn set_input(&self, input: Input) {
        let mut last = self.last_input.lock().unwrap();
        if *last != input {
            *last = input;
            unsafe { nagito_stream_set_input(self.raw, &input) };
        }
    }

    /// Refreshes network stats about once a second; call every frame.
    pub fn poll_stats(&self) {
        let mut at = self.stats_at.lock().unwrap();
        if at.elapsed() < Duration::from_secs(1) {
            return;
        }
        *at = Instant::now();
        let mut raw = RawStats::default();
        unsafe { nagito_stream_stats(self.raw, &mut raw) };
        let mut st = self.state.slot.stats.lock().unwrap();
        (st.rtt_ms, st.loss, st.bitrate_mbps, st.decode_ms) = (raw.rtt_ms, raw.loss, raw.bitrate_mbps, raw.decode_ms);
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.audio_stop.store(true, Ordering::Relaxed);
        unsafe { nagito_stream_stop(self.raw) }; // joins all native threads before StreamState is freed
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_id_must_be_8_bytes() {
        assert_eq!(super::parse_account_id("AQIDBAUGBwg="), Some([1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(super::parse_account_id("AQID"), None);
        assert_eq!(super::parse_account_id("not base64!"), None);
    }
}
