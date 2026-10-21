pub mod instruments;
pub mod pedals;
pub mod tascam;

use std::f32::consts::{PI, TAU};
use std::fs::File;
use std::path::Path;

/// Clamp a float between min and max
#[inline]
pub fn clamp(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

/// Fast rational approximation or standard tanh soft clipper
#[inline]
pub fn soft_clip(x: f32, drive: f32) -> f32 {
    if drive <= 0.001 {
        return x;
    }
    (x * drive).tanh() / drive.tanh().max(0.1)
}

/// Transformer saturation model: asymmetric 2nd/3rd harmonic soft knee above 0 dBV (1.0 V)
#[inline]
pub fn transformer_saturate(x: f32, supply_sag_factor: f32) -> f32 {
    let headroom = (1.0 * supply_sag_factor).max(0.25);
    let norm = x / headroom;
    // Subtle core hysteresis / asymmetry for 2nd harmonic warmth
    let biased = norm + 0.035 * norm * norm.abs();
    let sat = if biased.abs() <= 0.85 {
        biased
    } else {
        let sign = biased.signum();
        let excess = biased.abs() - 0.85;
        sign * (0.85 + 0.35 * (excess / 0.35).tanh())
    };
    sat * headroom
}

/// Simple deterministic PRNG (Xorshift32) for real-time audio thread noise without syscall locks
#[derive(Clone, Debug)]
pub struct FastRng {
    state: u32,
}

impl FastRng {
    pub fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Uniform white noise in [-1.0, 1.0]
    #[inline]
    pub fn white_noise(&mut self) -> f32 {
        let u = (self.next_u32() as f32) / (u32::MAX as f32);
        u * 2.0 - 1.0
    }
}

/// Paul Kellet's 7-state Pink Noise filter (-3 dB/octave)
#[derive(Clone, Debug, Default)]
pub struct PinkNoiseFilter {
    b0: f32,
    b1: f32,
    b2: f32,
    b3: f32,
    b4: f32,
    b5: f32,
    b6: f32,
}

impl PinkNoiseFilter {
    #[inline]
    pub fn process(&mut self, white: f32) -> f32 {
        self.b0 = 0.99886 * self.b0 + white * 0.0555179;
        self.b1 = 0.99332 * self.b1 + white * 0.0750759;
        self.b2 = 0.96900 * self.b2 + white * 0.1538520;
        self.b3 = 0.86650 * self.b3 + white * 0.3104856;
        self.b4 = 0.55000 * self.b4 + white * 0.5329522;
        self.b5 = -0.7616 * self.b5 - white * 0.0168980;
        let pink = self.b0
            + self.b1
            + self.b2
            + self.b3
            + self.b4
            + self.b5
            + self.b6
            + white * 0.5362;
        self.b6 = white * 0.115926;
        pink * 0.18
    }
}

/// Zero-Delay Feedback (ZDF) / Cytomic State Variable Filter (SVF)
/// Stable at high Q (100+) and right up to Nyquist.
#[derive(Clone, Debug)]
pub struct SvfFilter {
    ic1eq: f32,
    ic2eq: f32,
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Default for SvfFilter {
    fn default() -> Self {
        let mut s = Self {
            ic1eq: 0.0,
            ic2eq: 0.0,
            g: 0.0,
            k: 1.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
        };
        s.set_params(1000.0, 1.0, 48000.0);
        s
    }
}

impl SvfFilter {
    pub fn set_params(&mut self, cutoff_hz: f32, q: f32, sample_rate: f32) {
        let nyquist = sample_rate * 0.492;
        let fc = clamp(cutoff_hz, 0.1, nyquist);
        let q_clamped = clamp(q, 0.3, 250.0);
        self.g = (PI * fc / sample_rate).tan();
        self.k = 1.0 / q_clamped;
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    /// Returns (lowpass, bandpass_unity_gain, highpass, notch)
    #[inline]
    pub fn process_multimode(&mut self, v0: f32) -> (f32, f32, f32, f32) {
        let v3 = v0 - self.ic2eq;
        let v1 = self.a1 * self.ic1eq + self.a2 * v3;
        let v2 = self.ic2eq + self.a2 * self.ic1eq + self.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;

        // Avoid NaN/Inf explosions under extreme modulation
        if !self.ic1eq.is_finite() || !self.ic2eq.is_finite() {
            self.ic1eq = 0.0;
            self.ic2eq = 0.0;
            return (0.0, 0.0, 0.0, 0.0);
        }

        let lp = v2;
        // Unity-peak normalized bandpass: k * v1 has 0 dB gain at center frequency
        let bp_norm = self.k * v1;
        let hp = v0 - self.k * v1 - v2;
        let notch = v0 - self.k * v1;
        (lp, bp_norm, hp, notch)
    }

    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }
}

/// One-pole lowpass / highpass filter
#[derive(Clone, Debug, Default)]
pub struct OnePoleFilter {
    z1: f32,
    a0: f32,
    b1: f32,
}

impl OnePoleFilter {
    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: f32) {
        let fc = clamp(cutoff_hz, 0.0005, sample_rate * 0.49);
        self.b1 = (-TAU * fc / sample_rate).exp();
        self.a0 = 1.0 - self.b1;
    }

    pub fn set_time_constant(&mut self, tau_sec: f32, sample_rate: f32) {
        let tau = tau_sec.max(0.0001);
        self.b1 = (-1.0 / (tau * sample_rate)).exp();
        self.a0 = 1.0 - self.b1;
    }

    #[inline]
    pub fn process_lp(&mut self, x: f32) -> f32 {
        self.z1 = x * self.a0 + self.z1 * self.b1;
        self.z1
    }

    #[inline]
    pub fn process_hp(&mut self, x: f32) -> f32 {
        let lp = self.process_lp(x);
        x - lp
    }
}

/// Simulated electromechanical ballistic meter with needle mass & damping inertia
#[derive(Clone, Debug)]
pub struct BallisticMeter {
    pub rms_sq: f32,
    pub peak_env: f32,
    pub needle_pos: f32,
    pub needle_vel: f32,
}

impl Default for BallisticMeter {
    fn default() -> Self {
        Self {
            rms_sq: 0.0,
            peak_env: 0.0,
            needle_pos: 0.0,
            needle_vel: 0.0,
        }
    }
}

impl BallisticMeter {
    #[inline]
    pub fn process(&mut self, x: f32, fast_peak_mode: bool, sample_rate: f32) -> f32 {
        let dt = 1.0 / sample_rate.max(1000.0);
        let abs_x = x.abs();

        // Slow RMS detector (~300 ms integration)
        let alpha_rms = 1.0 - (-dt / 0.30).exp();
        self.rms_sq += alpha_rms * (x * x - self.rms_sq);
        let rms = self.rms_sq.max(0.0).sqrt();

        // Fast Peak detector (~2 ms attack, ~180 ms release)
        let alpha_att = 1.0 - (-dt / 0.002).exp();
        let alpha_rel = 1.0 - (-dt / 0.180).exp();
        if abs_x > self.peak_env {
            self.peak_env += alpha_att * (abs_x - self.peak_env);
        } else {
            self.peak_env += alpha_rel * (abs_x - self.peak_env);
        }

        let target = if fast_peak_mode { self.peak_env } else { rms };

        // 2nd-order damped spring-mass needle simulation
        let omega = if fast_peak_mode { 55.0 } else { 18.0 };
        let zeta = if fast_peak_mode { 0.65 } else { 0.85 };
        let accel = omega * omega * (target - self.needle_pos) - 2.0 * zeta * omega * self.needle_vel;
        self.needle_vel += accel * dt;
        self.needle_pos = clamp(self.needle_pos + self.needle_vel * dt, 0.0, 2.0);
        self.needle_pos
    }
}

/// Load an audio file (.wav, .mp3, .flac) from disk and resample/mix to mono at `target_sr`
pub fn load_audio_file_mono(path: &Path, target_sr: u32) -> anyhow::Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("No supported audio tracks in file"))?;
    let track_id = track.id;
    let src_sr = track.codec_params.sample_rate.unwrap_or(target_sr);
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut raw_mono = Vec::with_capacity(src_sr as usize * 10);

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        if let Ok(decoded) = decoder.decode(&packet) {
            let spec = *decoded.spec();
            let duration = decoded.capacity() as u64;
            let mut sample_buf = SampleBuffer::<f32>::new(duration, spec);
            sample_buf.copy_interleaved_ref(decoded);
            let samples = sample_buf.samples();
            for frame in samples.chunks(channels) {
                let sum: f32 = frame.iter().copied().sum();
                raw_mono.push(sum / (frame.len() as f32));
            }
        }
        // Cap maximum sample length to 60 seconds to keep memory bounded
        if raw_mono.len() > (src_sr as usize) * 60 {
            break;
        }
    }

    if raw_mono.is_empty() {
        anyhow::bail!("Decoded audio file contained 0 samples");
    }

    // Linear resampling if src_sr != target_sr
    if src_sr == target_sr {
        Ok(raw_mono)
    } else {
        let ratio = (src_sr as f64) / (target_sr as f64);
        let out_len = ((raw_mono.len() as f64) / ratio) as usize;
        let mut resampled = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let pos = (i as f64) * ratio;
            let idx = pos as usize;
            let frac = (pos - (idx as f64)) as f32;
            let s0 = raw_mono.get(idx).copied().unwrap_or(0.0);
            let s1 = raw_mono.get(idx + 1).copied().unwrap_or(s0);
            resampled.push(s0 + frac * (s1 - s0));
        }
        Ok(resampled)
    }
}
