use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;

use super::{clamp, soft_clip, OnePoleFilter, SvfFilter};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PedalId {
    Chorus,
    Delay,
    Outward,
    Reverb,
}

impl PedalId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Chorus => "ULTRA CHORUS UC-200",
            Self::Delay => "VINTAGE DELAY VD-400",
            Self::Outward => "COOPER FX OUTWARD MK2",
            Self::Reverb => "ALESIS WEDGE REVERB",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Chorus => "CHORUS",
            Self::Delay => "DELAY",
            Self::Outward => "OUTWARD",
            Self::Reverb => "WEDGE REV",
        }
    }
}

// ============================================================================
// 1. STEREO CHORUS (Behringer Ultra Chorus style)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChorusParams {
    pub bypass: bool,
    pub rate_hz: f32,  // 0.1 to 8.0 Hz
    pub depth: f32,    // 0.0 to 1.0 (modulates 5 ms .. 30 ms delay range)
    pub tone: f32,     // 0.0 (dark) to 1.0 (bright) EQ
    pub wet_dry: f32,  // 0.0 to 1.0
}

impl Default for ChorusParams {
    fn default() -> Self {
        Self {
            bypass: false,
            rate_hz: 0.85,
            depth: 0.45,
            tone: 0.65,
            wet_dry: 0.40,
        }
    }
}

pub struct StereoChorus {
    pub params: ChorusParams,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    lfo_phase: f32,
    tone_l: SvfFilter,
    tone_r: SvfFilter,
}

impl Default for StereoChorus {
    fn default() -> Self {
        // 50 ms buffer at 96kHz max
        let cap = 4800;
        Self {
            params: ChorusParams::default(),
            buf_l: vec![0.0; cap],
            buf_r: vec![0.0; cap],
            write_pos: 0,
            lfo_phase: 0.0,
            tone_l: SvfFilter::default(),
            tone_r: SvfFilter::default(),
        }
    }
}

impl StereoChorus {
    pub fn process(&mut self, in_l: f32, in_r: f32, sample_rate: f32) -> (f32, f32) {
        let len = self.buf_l.len();
        self.buf_l[self.write_pos] = in_l;
        self.buf_r[self.write_pos] = in_r;

        let rate = clamp(self.params.rate_hz, 0.05, 10.0);
        self.lfo_phase = (self.lfo_phase + rate / sample_rate).fract();

        // Anti-phase LFOs driving 5 ms to 30 ms modulated delay lines
        let lfo_l = (self.lfo_phase * TAU).sin();
        let lfo_r = -lfo_l;

        let center_ms = 17.5; // midpoint of 5..30 ms
        let swing_ms = 12.5 * clamp(self.params.depth, 0.0, 1.0);
        let delay_l_samps = ((center_ms + lfo_l * swing_ms) * 0.001 * sample_rate).clamp(1.0, (len - 2) as f32);
        let delay_r_samps = ((center_ms + lfo_r * swing_ms) * 0.001 * sample_rate).clamp(1.0, (len - 2) as f32);

        let wet_l = read_interp(&self.buf_l, self.write_pos, delay_l_samps);
        let wet_r = read_interp(&self.buf_r, self.write_pos, delay_r_samps);

        self.write_pos = (self.write_pos + 1) % len;

        if self.params.bypass {
            return (in_l, in_r);
        }

        // Tone/EQ filter on wet chorus path (400 Hz to 12 kHz)
        let cutoff = 400.0 * (30.0_f32).powf(clamp(self.params.tone, 0.0, 1.0));
        self.tone_l.set_params(cutoff, 0.707, sample_rate);
        self.tone_r.set_params(cutoff, 0.707, sample_rate);
        let (eq_l, _, _, _) = self.tone_l.process_multimode(wet_l);
        let (eq_r, _, _, _) = self.tone_r.process_multimode(wet_r);

        let mix = clamp(self.params.wet_dry, 0.0, 1.0);
        (
            in_l * (1.0 - mix) + eq_l * mix,
            in_r * (1.0 - mix) + eq_r * mix,
        )
    }
}

// ============================================================================
// 2. STEREO DIGITAL DELAY (Behringer Vintage Delay style)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelayParams {
    pub bypass: bool,
    pub delay_ms: f32,      // 10.0 to 1500.0 ms
    pub feedback: f32,      // 0.0 to 1.15 (>1.0 enters warm BBD self-oscillation)
    pub ping_pong: bool,    // Stereo ping-pong cross-feedback
    pub low_cut_hz: f32,    // 20 Hz to 800 Hz damping
    pub high_cut_hz: f32,   // 800 Hz to 14,000 Hz damping
    pub mix: f32,           // 0.0 to 1.0
}

impl Default for DelayParams {
    fn default() -> Self {
        Self {
            bypass: false,
            delay_ms: 365.0,
            feedback: 0.52,
            ping_pong: true,
            low_cut_hz: 110.0,
            high_cut_hz: 3200.0,
            mix: 0.35,
        }
    }
}

pub struct StereoDigitalDelay {
    pub params: DelayParams,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    smooth_delay_samps: f32,
    hp_l: OnePoleFilter,
    hp_r: OnePoleFilter,
    lp_l: OnePoleFilter,
    lp_r: OnePoleFilter,
}

impl Default for StereoDigitalDelay {
    fn default() -> Self {
        let cap = 96_000 * 2; // Up to 2 seconds at 96kHz
        Self {
            params: DelayParams::default(),
            buf_l: vec![0.0; cap],
            buf_r: vec![0.0; cap],
            write_pos: 0,
            smooth_delay_samps: 17520.0,
            hp_l: OnePoleFilter::default(),
            hp_r: OnePoleFilter::default(),
            lp_l: OnePoleFilter::default(),
            lp_r: OnePoleFilter::default(),
        }
    }
}

impl StereoDigitalDelay {
    pub fn process(&mut self, in_l: f32, in_r: f32, sample_rate: f32) -> (f32, f32) {
        let len = self.buf_l.len();
        let target_samps = clamp(self.params.delay_ms, 10.0, 1500.0) * 0.001 * sample_rate;
        // Smooth delay time changes for analog pitch-shift glide on knob turn
        self.smooth_delay_samps += 0.0015 * (target_samps - self.smooth_delay_samps);
        let delay_samps = self.smooth_delay_samps.clamp(1.0, (len - 4) as f32);

        let tap_l = read_interp(&self.buf_l, self.write_pos, delay_samps);
        let tap_r = read_interp(&self.buf_r, self.write_pos, delay_samps);

        // High-cut and Low-cut damping filters in feedback path
        let lc = clamp(self.params.low_cut_hz, 20.0, 1000.0);
        let hc = clamp(self.params.high_cut_hz, 500.0, 16000.0);
        self.hp_l.set_cutoff(lc, sample_rate);
        self.hp_r.set_cutoff(lc, sample_rate);
        self.lp_l.set_cutoff(hc, sample_rate);
        self.lp_r.set_cutoff(hc, sample_rate);

        let filt_l = self.lp_l.process_lp(self.hp_l.process_hp(tap_l));
        let filt_r = self.lp_r.process_lp(self.hp_r.process_hp(tap_r));

        let fb = clamp(self.params.feedback, 0.0, 1.18);
        let (next_l, next_r) = if self.params.ping_pong {
            (
                soft_clip(in_l + filt_r * fb, 1.15),
                soft_clip(in_r * 0.35 + filt_l * fb, 1.15),
            )
        } else {
            (
                soft_clip(in_l + filt_l * fb, 1.15),
                soft_clip(in_r + filt_r * fb, 1.15),
            )
        };

        self.buf_l[self.write_pos] = next_l;
        self.buf_r[self.write_pos] = next_r;
        self.write_pos = (self.write_pos + 1) % len;

        if self.params.bypass {
            return (in_l, in_r);
        }

        let mix = clamp(self.params.mix, 0.0, 1.0);
        (
            in_l * (1.0 - mix) + tap_l * mix,
            in_r * (1.0 - mix) + tap_r * mix,
        )
    }
}

// ============================================================================
// 3. COOPER FX OUTWARD MK2 EMULATION (Micro-Looping Granular / Time-Stretch)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutwardMode {
    TimeStretch,
    Freeze,
    GlitchStutter,
}

impl OutwardMode {
    pub fn next(self) -> Self {
        match self {
            Self::TimeStretch => Self::Freeze,
            Self::Freeze => Self::GlitchStutter,
            Self::GlitchStutter => Self::TimeStretch,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TimeStretch => "TIME STRETCH",
            Self::Freeze => "FREEZE BUFFER",
            Self::GlitchStutter => "GLITCH / STUTTER",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutwardParams {
    pub bypass: bool,
    pub mode: OutwardMode,
    pub grain_size_ms: f32,     // 5.0 to 500.0 ms
    pub pitch_semitones: f32,   // -12.0 to +12.0 semitones
    pub stretch_density: f32,   // 0.1 to 1.0
    pub mix: f32,               // 0.0 to 1.0
}

impl Default for OutwardParams {
    fn default() -> Self {
        Self {
            bypass: false,
            mode: OutwardMode::TimeStretch,
            grain_size_ms: 85.0,
            pitch_semitones: 0.0,
            stretch_density: 0.65,
            mix: 0.35,
        }
    }
}

pub struct OutwardGranular {
    pub params: OutwardParams,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_pos: usize,
    grain_phase: f32,
    stutter_counter: usize,
    glitch_offset: f32,
}

impl Default for OutwardGranular {
    fn default() -> Self {
        let cap = 96_000; // 1-2s granular capture buffer
        Self {
            params: OutwardParams::default(),
            buf_l: vec![0.0; cap],
            buf_r: vec![0.0; cap],
            write_pos: 0,
            grain_phase: 0.0,
            stutter_counter: 0,
            glitch_offset: 2400.0,
        }
    }
}

impl OutwardGranular {
    pub fn process(&mut self, in_l: f32, in_r: f32, sample_rate: f32) -> (f32, f32) {
        let len = self.buf_l.len();

        // Only write new audio into capture buffer when not in Freeze mode
        if self.params.mode != OutwardMode::Freeze {
            self.buf_l[self.write_pos] = in_l;
            self.buf_r[self.write_pos] = in_r;
            self.write_pos = (self.write_pos + 1) % len;
        }

        if self.params.bypass {
            return (in_l, in_r);
        }

        let grain_samps = (clamp(self.params.grain_size_ms, 5.0, 500.0) * 0.001 * sample_rate)
            .clamp(64.0, (len / 2) as f32);
        let pitch_ratio = (2.0_f32).powf(clamp(self.params.pitch_semitones, -12.0, 12.0) / 12.0);

        // Advance grain phase based on pitch shift and mode
        let rate_step = match self.params.mode {
            OutwardMode::TimeStretch => (pitch_ratio - 1.0).abs().max(0.25) / grain_samps,
            OutwardMode::Freeze => pitch_ratio / grain_samps,
            OutwardMode::GlitchStutter => {
                self.stutter_counter = self.stutter_counter.wrapping_add(1);
                let period = (sample_rate * (1.1 - clamp(self.params.stretch_density, 0.1, 1.0) * 0.9)) as usize;
                if self.stutter_counter % period.max(1200) == 0 {
                    // Pseudo-random jump in capture buffer
                    self.glitch_offset = ((self.stutter_counter * 1103515245 + 12345) % (len / 3)) as f32 + 256.0;
                }
                pitch_ratio / grain_samps
            }
        };

        self.grain_phase = (self.grain_phase + rate_step).fract();

        // Two overlapping Hann-windowed grains (180 degrees out of phase)
        let p0 = self.grain_phase;
        let p1 = (self.grain_phase + 0.5).fract();
        let w0 = 0.5 * (1.0 - (p0 * TAU).cos());
        let w1 = 0.5 * (1.0 - (p1 * TAU).cos());

        let base_delay = match self.params.mode {
            OutwardMode::GlitchStutter => self.glitch_offset,
            _ => grain_samps * clamp(self.params.stretch_density, 0.1, 1.0),
        };

        let d0 = (base_delay + (1.0 - p0 * pitch_ratio) * grain_samps).clamp(2.0, (len - 4) as f32);
        let d1 = (base_delay + (1.0 - p1 * pitch_ratio) * grain_samps).clamp(2.0, (len - 4) as f32);

        let g_l = read_interp(&self.buf_l, self.write_pos, d0) * w0
            + read_interp(&self.buf_l, self.write_pos, d1) * w1;
        let g_r = read_interp(&self.buf_r, self.write_pos, d0 * 1.03) * w0
            + read_interp(&self.buf_r, self.write_pos, d1 * 1.03) * w1;

        let mix = clamp(self.params.mix, 0.0, 1.0);
        (
            in_l * (1.0 - mix) + g_l * mix,
            in_r * (1.0 - mix) + g_r * mix,
        )
    }
}

// ============================================================================
// 4. ALESIS WEDGE REVERB EMULATION (All-Pass Diffusion + 8-Line FDN)
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WedgeReverbParams {
    pub bypass: bool,
    pub room_size: f32,     // 0.1 to 1.0
    pub decay_time: f32,    // 0.1 to 0.98
    pub pre_delay_ms: f32,  // 0.0 to 150.0 ms
    pub hf_damping: f32,    // 0.0 (bright) to 0.95 (warm/dark)
    pub mix: f32,           // 0.0 to 1.0
}

impl Default for WedgeReverbParams {
    fn default() -> Self {
        Self {
            bypass: false,
            room_size: 0.72,
            decay_time: 0.78,
            pre_delay_ms: 24.0,
            hf_damping: 0.45,
            mix: 0.32,
        }
    }
}

struct AllPassDiffuser {
    buf: Vec<f32>,
    pos: usize,
    g: f32,
}

impl AllPassDiffuser {
    fn new(len: usize, g: f32) -> Self {
        Self {
            buf: vec![0.0; len.max(8)],
            pos: 0,
            g,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let delayed = self.buf[self.pos];
        let v = x - self.g * delayed;
        self.buf[self.pos] = v;
        self.pos = (self.pos + 1) % self.buf.len();
        delayed + self.g * v
    }
}

pub struct WedgeReverb {
    pub params: WedgeReverbParams,
    pre_delay_buf: Vec<f32>,
    pre_pos: usize,
    diffusers: [AllPassDiffuser; 4],
    fdn_buffers: [Vec<f32>; 8],
    fdn_pos: [usize; 8],
    fdn_damp: [f32; 8],
}

const FDN_BASE_PRIMES: [usize; 8] = [1117, 1361, 1609, 1879, 2143, 2477, 2803, 3251];

impl Default for WedgeReverb {
    fn default() -> Self {
        Self {
            params: WedgeReverbParams::default(),
            pre_delay_buf: vec![0.0; 16000],
            pre_pos: 0,
            diffusers: [
                AllPassDiffuser::new(149, 0.70),
                AllPassDiffuser::new(241, 0.70),
                AllPassDiffuser::new(389, 0.65),
                AllPassDiffuser::new(577, 0.65),
            ],
            fdn_buffers: FDN_BASE_PRIMES.map(|n| vec![0.0; n * 2]),
            fdn_pos: [0; 8],
            fdn_damp: [0.0; 8],
        }
    }
}

impl WedgeReverb {
    pub fn process(&mut self, in_l: f32, in_r: f32, sample_rate: f32) -> (f32, f32) {
        let mono_in = (in_l + in_r) * 0.5;

        // 1. Pre-Delay
        let pre_len = self.pre_delay_buf.len();
        self.pre_delay_buf[self.pre_pos] = mono_in;
        let pre_samps = ((clamp(self.params.pre_delay_ms, 0.0, 150.0) * 0.001 * sample_rate) as usize)
            .min(pre_len - 2);
        let read_pre = (self.pre_pos + pre_len - pre_samps) % pre_len;
        let pre_out = self.pre_delay_buf[read_pre];
        self.pre_pos = (self.pre_pos + 1) % pre_len;

        // 2. 4-Stage All-Pass Input Diffusion Network
        let mut diff = pre_out;
        for ap in &mut self.diffusers {
            diff = ap.process(diff);
        }

        // 3. 8-Line Feedback Delay Network (FDN) with Householder/Hadamard orthogonal matrix feedback
        let size_scale = clamp(self.params.room_size, 0.15, 1.0);
        let decay = clamp(self.params.decay_time, 0.05, 0.97);
        let damp = clamp(self.params.hf_damping, 0.02, 0.95);

        let mut taps = [0.0_f32; 8];
        for i in 0..8 {
            let max_len = self.fdn_buffers[i].len();
            let active_len = ((FDN_BASE_PRIMES[i] as f32 * (0.4 + 1.4 * size_scale)) as usize)
                .clamp(64, max_len - 1);
            let idx = self.fdn_pos[i] % active_len;
            let raw_tap = self.fdn_buffers[i][idx];
            // One-pole high-frequency damping filter per delay line
            self.fdn_damp[i] = raw_tap * (1.0 - damp) + self.fdn_damp[i] * damp;
            taps[i] = self.fdn_damp[i];
        }

        // Householder reflection matrix: y_i = x_i - (2/N) * sum(x)
        let tap_sum: f32 = taps.iter().sum::<f32>() * 0.25;

        for i in 0..8 {
            let max_len = self.fdn_buffers[i].len();
            let active_len = ((FDN_BASE_PRIMES[i] as f32 * (0.4 + 1.4 * size_scale)) as usize)
                .clamp(64, max_len - 1);
            let idx = self.fdn_pos[i] % active_len;
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            let fb_val = (taps[i] - tap_sum) * decay + diff * 0.25 * sign;
            self.fdn_buffers[i][idx] = soft_clip(fb_val, 1.0);
            self.fdn_pos[i] = (idx + 1) % active_len;
        }

        if self.params.bypass {
            return (in_l, in_r);
        }

        let wet_l = (taps[0] + taps[2] - taps[4] + taps[6]) * 0.45;
        let wet_r = (taps[1] - taps[3] + taps[5] + taps[7]) * 0.45;
        let mix = clamp(self.params.mix, 0.0, 1.0);

        (
            in_l * (1.0 - mix) + wet_l * mix,
            in_r * (1.0 - mix) + wet_r * mix,
        )
    }
}

// ============================================================================
// MODULAR REORDERABLE PEDALBOARD RACK
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PedalboardState {
    pub order: [PedalId; 4],
    pub chorus: ChorusParams,
    pub delay: DelayParams,
    pub outward: OutwardParams,
    pub reverb: WedgeReverbParams,
}

impl Default for PedalboardState {
    fn default() -> Self {
        Self {
            order: [
                PedalId::Chorus,
                PedalId::Outward,
                PedalId::Delay,
                PedalId::Reverb,
            ],
            chorus: ChorusParams::default(),
            delay: DelayParams::default(),
            outward: OutwardParams::default(),
            reverb: WedgeReverbParams::default(),
        }
    }
}

pub struct Pedalboard {
    pub order: [PedalId; 4],
    pub chorus: StereoChorus,
    pub delay: StereoDigitalDelay,
    pub outward: OutwardGranular,
    pub reverb: WedgeReverb,
}

impl Default for Pedalboard {
    fn default() -> Self {
        Self {
            order: [
                PedalId::Chorus,
                PedalId::Outward,
                PedalId::Delay,
                PedalId::Reverb,
            ],
            chorus: StereoChorus::default(),
            delay: StereoDigitalDelay::default(),
            outward: OutwardGranular::default(),
            reverb: WedgeReverb::default(),
        }
    }
}

impl Pedalboard {
    pub fn swap_slots(&mut self, slot_a: usize, slot_b: usize) {
        if slot_a < 4 && slot_b < 4 {
            self.order.swap(slot_a, slot_b);
        }
    }

    pub fn snapshot(&self) -> PedalboardState {
        PedalboardState {
            order: self.order,
            chorus: self.chorus.params.clone(),
            delay: self.delay.params.clone(),
            outward: self.outward.params.clone(),
            reverb: self.reverb.params.clone(),
        }
    }

    pub fn process(&mut self, mut l: f32, mut r: f32, sample_rate: f32) -> (f32, f32) {
        for &pedal in &self.order {
            let (nl, nr) = match pedal {
                PedalId::Chorus => self.chorus.process(l, r, sample_rate),
                PedalId::Delay => self.delay.process(l, r, sample_rate),
                PedalId::Outward => self.outward.process(l, r, sample_rate),
                PedalId::Reverb => self.reverb.process(l, r, sample_rate),
            };
            l = nl;
            r = nr;
        }
        (l, r)
    }
}

#[inline]
fn read_interp(buf: &[f32], write_pos: usize, delay_samples: f32) -> f32 {
    let len = buf.len();
    let read_f = (write_pos as f32) - delay_samples + (len as f32) * 2.0;
    let idx0 = (read_f as usize) % len;
    let idx1 = (idx0 + 1) % len;
    let frac = read_f.fract();
    buf[idx0] + frac * (buf[idx1] - buf[idx0])
}
