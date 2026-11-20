//! Capture card source: Media Foundation video via nokhwa, audio passthrough via cpal.

use crate::video::{Frame, FrameSlot, Pixels};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq)]
pub struct VideoDevice {
    pub index: u32,
    pub name: String,
}

pub fn video_devices() -> Vec<VideoDevice> {
    nokhwa::query(ApiBackend::MediaFoundation)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| match c.index() {
            CameraIndex::Index(i) => Some(VideoDevice { index: *i, name: c.human_name() }),
            _ => None,
        })
        .collect()
}

pub fn audio_inputs() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| d.description().ok().map(|x| x.name().to_string())).collect())
        .unwrap_or_default()
}

/// Best guess at the capture card's audio input, matched by name.
pub fn guess_audio(video_name: &str, inputs: &[String]) -> Option<String> {
    const HINTS: [&str; 8] = ["capture", "usb video", "hdmi", "cam link", "elgato", "avermedia", "digital audio interface", "ms2109"];
    let video = video_name.to_lowercase();
    inputs
        .iter()
        .find(|n| {
            let n = n.to_lowercase();
            HINTS.iter().any(|h| n.contains(h)) || video.split_whitespace().any(|w| w.len() > 3 && n.contains(w))
        })
        .cloned()
}

/// A picture mode the card offers: width, height, fps.
pub type CamMode = (u32, u32, u32);

pub struct Session {
    stop: Arc<AtomicBool>,
    /// Modes the card reports, filled once it opens.
    pub modes: Arc<Mutex<Vec<CamMode>>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn start(dev: VideoDevice, mode: Option<CamMode>, audio: Option<String>, slot: Arc<FrameSlot>, repaint: impl Fn() + Send + 'static) -> Session {
    let stop = Arc::new(AtomicBool::new(false));
    let modes = Arc::new(Mutex::new(vec![]));
    {
        let (stop, slot, modes) = (stop.clone(), slot.clone(), modes.clone());
        std::thread::spawn(move || {
            if let Err(e) = video_loop(dev, mode, &modes, &slot, &stop, &repaint) {
                slot.stats.lock().unwrap().error = Some(format!("Capture card stopped: {e}"));
                repaint();
            }
        });
    }
    if let Some(name) = audio {
        let (stop, slot) = (stop.clone(), slot.clone());
        std::thread::spawn(move || {
            let msg = match audio_loop(&name, &stop) {
                Ok(()) => return,
                Err(e) => format!("No sound from {name}: {e}"),
            };
            slot.stats.lock().unwrap().audio = Some(msg);
        });
    }
    Session { stop, modes }
}

/// Dev aid: a 1280x720 square sliding across a grid at 30 fps.
pub fn test_pattern(slot: Arc<FrameSlot>, repaint: impl Fn() + Send + 'static) -> Session {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    std::thread::spawn(move || {
        let (w, h) = (1280u32, 720u32);
        let start = Instant::now();
        for n in 0u32.. {
            if s.load(Ordering::Relaxed) {
                break;
            }
            let x0 = (n * 24) % (w - 160);
            let mut data = vec![40u8; (w * h * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    let grid = (x / 40 + y / 40) % 2 == 0;
                    let square = x >= x0 && x < x0 + 160 && (280..440).contains(&y);
                    let v = if square { [242, 165, 65, 255] } else if grid { [60, 58, 90, 255] } else { [30, 28, 48, 255] };
                    data[i..i + 4].copy_from_slice(&v);
                }
            }
            slot.push(Frame { width: w, height: h, pixels: Pixels::Rgba, data, captured_at: Instant::now() });
            {
                let mut st = slot.stats.lock().unwrap();
                (st.width, st.height, st.source_fps) = (w, h, 30.0);
            }
            repaint();
            let next = start + Duration::from_secs_f64((n + 1) as f64 / 30.0);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    });
    Session { stop, modes: Arc::new(Mutex::new(vec![])) }
}

fn video_loop(dev: VideoDevice, mode: Option<CamMode>, modes: &Mutex<Vec<CamMode>>, slot: &FrameSlot, stop: &AtomicBool, repaint: &dyn Fn()) -> anyhow::Result<()> {
    let fmt = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut cam = nokhwa::Camera::new(CameraIndex::Index(dev.index), fmt)?;
    let formats = cam.compatible_camera_formats().unwrap_or_default();
    let mut list: Vec<CamMode> = formats.iter().map(|f| (f.width(), f.height(), f.frame_rate())).collect();
    list.sort_by(|a, b| (b.1, b.2).cmp(&(a.1, a.2)));
    list.dedup();
    *modes.lock().unwrap() = list;
    if let Some((w, h, fps)) = mode {
        // NV12 goes straight to the GPU; YUYV is cheap to unpack; MJPEG needs a full decode.
        let rank = |f: &FrameFormat| match f {
            FrameFormat::NV12 => 0,
            FrameFormat::YUYV => 1,
            _ => 2,
        };
        if let Some(f) = formats.iter().filter(|f| (f.width(), f.height(), f.frame_rate()) == (w, h, fps)).min_by_key(|f| rank(&f.format())) {
            cam.set_camera_requset(RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Exact(*f)))?;
        }
    }
    cam.open_stream()?;
    let (mut frames, mut window) = (0u32, Instant::now());
    while !stop.load(Ordering::Relaxed) {
        let buf = cam.frame()?;
        let captured_at = Instant::now();
        let (width, height) = (buf.resolution().width(), buf.resolution().height());
        let frame = if buf.source_frame_format() == FrameFormat::NV12 && buf.buffer().len() >= (width * height * 3 / 2) as usize {
            // shortcut: assumes limited-range BT.709, what HDMI capture cards send for HD; add a toggle if colors look off.
            let data = buf.buffer()[..(width * height * 3 / 2) as usize].to_vec();
            Frame { width, height, pixels: Pixels::Nv12 { full_range: false, bt601: false }, data, captured_at }
        } else {
            let img = buf.decode_image::<RgbAFormat>()?;
            Frame { width: img.width(), height: img.height(), pixels: Pixels::Rgba, data: img.into_raw(), captured_at }
        };
        let (width, height) = (frame.width, frame.height);
        let decode_ms = captured_at.elapsed().as_secs_f32() * 1000.0;
        slot.push(frame);
        frames += 1;
        {
            let mut s = slot.stats.lock().unwrap();
            s.decode_ms = decode_ms;
            (s.width, s.height) = (width, height);
            if window.elapsed() >= Duration::from_secs(1) {
                s.source_fps = frames as f32 / window.elapsed().as_secs_f32();
                (frames, window) = (0, Instant::now());
            }
        }
        repaint();
    }
    cam.stop_stream()?;
    Ok(())
}

/// Most audio we allow to queue before dropping old samples, so sound stays in sync with the picture.
const MAX_AUDIO_MS: usize = 40;

fn audio_loop(name: &str, stop: &AtomicBool) -> anyhow::Result<()> {
    let input = cpal::default_host()
        .input_devices()?
        .find(|d| d.description().map(|x| x.name() == name).unwrap_or(false))
        .ok_or_else(|| anyhow::anyhow!("device not found"))?;
    let icfg = input.default_input_config()?;
    anyhow::ensure!(icfg.sample_format() == cpal::SampleFormat::F32, "unsupported sample format");
    let ich = icfg.channels() as usize;
    let q = Arc::new(Mutex::new(VecDeque::<f32>::new()));
    let qi = q.clone();
    let rate = icfg.sample_rate();
    let in_stream = input.build_input_stream::<f32, _, _>(
        icfg.config(),
        move |data, _| push_audio(&qi, data.iter().copied(), rate, ich),
        |e| eprintln!("audio input: {e}"),
        None,
    )?;
    let out_stream = output_stream(q, rate, ich)?;
    in_stream.play()?;
    out_stream.play()?;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

static VOLUME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3F80_0000); // 1.0f32

pub fn set_volume(v: f32) {
    VOLUME.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

pub type AudioQueue = Arc<Mutex<VecDeque<f32>>>;

/// Append interleaved samples, dropping the oldest beyond MAX_AUDIO_MS so sound never lags the picture.
pub fn push_audio(q: &Mutex<VecDeque<f32>>, data: impl Iterator<Item = f32>, rate: u32, ch: usize) {
    let max = rate as usize * MAX_AUDIO_MS / 1000 * ch;
    let mut q = q.lock().unwrap();
    q.extend(data);
    let excess = q.len().saturating_sub(max);
    q.drain(..excess - excess % ch);
}

/// Plays a queue of interleaved `ich`-channel samples at `in_rate` on the default output device.
pub fn output_stream(q: AudioQueue, in_rate: u32, ich: usize) -> anyhow::Result<cpal::Stream> {
    let output = cpal::default_host().default_output_device().ok_or_else(|| anyhow::anyhow!("no speakers or headphones found"))?;
    let ocfg = output.default_output_config()?;
    anyhow::ensure!(ocfg.sample_format() == cpal::SampleFormat::F32, "unsupported output sample format");
    let och = ocfg.channels() as usize;
    let ratio = in_rate as f64 / ocfg.sample_rate() as f64;
    let mut pos = 0.0f64; // fractional read position in input frames
    Ok(output.build_output_stream::<f32, _, _>(
        ocfg.config(),
        move |out, _| {
            let gain = f32::from_bits(VOLUME.load(Ordering::Relaxed));
            let mut q = q.lock().unwrap();
            for frame in out.chunks_mut(och) {
                let i = pos as usize * ich;
                if i + ich > q.len() {
                    frame.fill(0.0);
                    continue;
                }
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = q[i + c.min(ich - 1)] * gain;
                }
                pos += ratio;
            }
            // shortcut: nearest-sample resampling when rates differ; add a proper resampler if it sounds rough.
            let used = (pos as usize * ich).min(q.len());
            q.drain(..used);
            pos = pos.fract();
        },
        |e| eprintln!("audio output: {e}"),
        None,
    )?)
}
#[cfg(test)]
mod tests {
    #[test]
    fn guesses_capture_audio() {
        let inputs = vec!["Microphone (HyperX SoloCast)".into(), "Digital Audio Interface (USB3.0 Capture)".into()];
        assert_eq!(super::guess_audio("USB3.0 Capture", &inputs).as_deref(), Some("Digital Audio Interface (USB3.0 Capture)"));
        assert_eq!(super::guess_audio("Webcam", &inputs[..1]), None);
    }
}
