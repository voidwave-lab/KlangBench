// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 KlangBench Audio Lab
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

use serde::{Deserialize, Serialize};
use std::f32::consts::{PI, TAU};

use super::{
    clamp, soft_clip, transformer_saturate, BallisticMeter, FastRng, OnePoleFilter,
    PinkNoiseFilter, SvfFilter,
};

// ============================================================================
// 1. TAG-101 AUDIO SIGNAL GENERATOR (Wien Bridge Oscillator)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tag101Decade {
    X1,    // 10 Hz - 100 Hz
    X10,   // 100 Hz - 1 kHz
    X100,  // 1 kHz - 10 kHz
    X1k,   // 10 kHz - 100 kHz
    X10k,  // 100 kHz - 1 MHz (ultrasonic / heterodyne)
    X100k, // 1 MHz range
}

impl Tag101Decade {
    pub fn multiplier(self) -> f32 {
        match self {
            Self::X1 => 1.0,
            Self::X10 => 10.0,
            Self::X100 => 100.0,
            Self::X1k => 1_000.0,
            Self::X10k => 10_000.0,
            Self::X100k => 100_000.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::X1 => "x1",
            Self::X10 => "x10",
            Self::X100 => "x100",
            Self::X1k => "x1k",
            Self::X10k => "x10k",
            Self::X100k => "x100k",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::X1 => Self::X10,
            Self::X10 => Self::X100,
            Self::X100 => Self::X1k,
            Self::X1k => Self::X10k,
            Self::X10k => Self::X100k,
            Self::X100k => Self::X1,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::X1 => Self::X100k,
            Self::X10 => Self::X1,
            Self::X100 => Self::X10,
            Self::X1k => Self::X100,
            Self::X10k => Self::X1k,
            Self::X100k => Self::X10k,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tag101Waveform {
    Sine,
    Square,
}

impl Tag101Waveform {
    pub fn toggle(self) -> Self {
        match self {
            Self::Sine => Self::Square,
            Self::Square => Self::Sine,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tag101Params {
    pub enabled: bool,
    pub dial_freq: f32, // 10.0 to 100.0
    pub decade: Tag101Decade,
    pub waveform: Tag101Waveform,
    pub attenuator_db: i32, // 0, -10, -20, -30, -40, -50
    pub fine_level: f32,    // 0.0 to 1.35 (exceeds 0 dBV above 1.0)
}

impl Default for Tag101Params {
    fn default() -> Self {
        Self {
            enabled: true,
            dial_freq: 22.0, // 220 Hz at x10
            decade: Tag101Decade::X10,
            waveform: Tag101Waveform::Sine,
            attenuator_db: 0,
            fine_level: 0.95,
        }
    }
}

impl Tag101Params {
    pub fn nominal_frequency_hz(&self) -> f32 {
        clamp(self.dial_freq * self.decade.multiplier(), 10.0, 1_000_000.0)
    }
}

pub struct Tag101Generator {
    pub params: Tag101Params,
    phase: f64,
    bulb_resistance_env: f32,
    last_freq: f32,
    slew_state: f32,
}

impl Default for Tag101Generator {
    fn default() -> Self {
        Self {
            params: Tag101Params::default(),
            phase: 0.0,
            bulb_resistance_env: 1.0,
            last_freq: 220.0,
            slew_state: 0.0,
        }
    }
}

impl Tag101Generator {
    pub fn process(&mut self, cv_mod: f32, supply_sag: f32, sample_rate: f32) -> (f32, f32) {
        if !self.params.enabled {
            return (0.0, 0.0);
        }

        let base_freq = self.params.nominal_frequency_hz();
        // Apply CV modulation (±1 octave full scale) and slight supply sag pitch drift
        let sag_drift = 1.0 - (1.0 - supply_sag) * 0.015;
        let true_freq = clamp(
            base_freq * (2.0_f32).powf(cv_mod) * sag_drift,
            10.0,
            1_000_000.0,
        );

        // Simulate Wien bridge incandescent thermistor lamp settling bounce on rapid tuning changes
        let freq_delta = ((true_freq - self.last_freq).abs() / true_freq.max(10.0)).min(1.0);
        self.last_freq = true_freq;
        self.bulb_resistance_env += freq_delta * 0.08;
        let dt = 1.0 / sample_rate;
        self.bulb_resistance_env += (1.0 - self.bulb_resistance_env) * (1.0 - (-dt / 0.12).exp());
        self.bulb_resistance_env = clamp(self.bulb_resistance_env, 0.85, 1.25);

        // Effective audio oscillator frequency (when in ultrasonic x10k/x100k decades, model
        // analog sub-harmonic divider / aliasing foldback at low level while preserving true phase)
        let nyquist = sample_rate * 0.48;
        let (eff_freq, ultrasonic_att) = if true_freq <= nyquist {
            (true_freq, 1.0)
        } else {
            // Folded heterodyne alias in audio band so ultrasonic tuning produces eerie B&K / Hainbach beat tones
            let folded = (true_freq % nyquist).max(40.0);
            (folded, 0.28)
        };

        let dt_phase = (eff_freq as f64) / (sample_rate as f64);
        self.phase = (self.phase + dt_phase).fract();
        let p = self.phase as f32;

        let raw_wave = match self.params.waveform {
            Tag101Waveform::Sine => {
                let s = (p * TAU).sin();
                // Subtle Wien bridge 2nd & 3rd harmonic THD (~0.15%)
                let s2 = (p * 2.0 * TAU).sin() * 0.0012;
                let s3 = (p * 3.0 * TAU).sin() * 0.0008;
                (s + s2 + s3) * self.bulb_resistance_env
            }
            Tag101Waveform::Square => {
                // PolyBLEP anti-aliased fast rise-time square wave
                let t = p;
                let dt_f = dt_phase as f32;
                let mut sq = if t < 0.5 { 1.0 } else { -1.0 };
                sq += poly_blep(t, dt_f);
                sq -= poly_blep((t + 0.5).fract(), dt_f);
                // Fast analog rise-time slew filter
                self.slew_state += 0.65 * (sq - self.slew_state);
                self.slew_state
            }
        };

        let att_db = clamp(self.params.attenuator_db as f32, -50.0, 0.0);
        let att_lin = 10.0_f32.powf(att_db / 20.0);
        let pre_xfmr = raw_wave * ultrasonic_att * att_lin * self.params.fine_level;

        // Mild analog transformer saturation when output exceeds 0 dBV (1.0 V)
        let out = transformer_saturate(pre_xfmr, supply_sag);
        (out, p)
    }
}

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

// ============================================================================
// 2. BRÜEL & KJÆR 2107 FREQUENCY ANALYZER
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bk2107Params {
    pub enabled: bool,
    pub center_freq_hz: f32,   // 20 Hz to 20,000 Hz
    pub q_factor: f32,         // 1.0 to 120.0 (ultra-narrow variable-Q)
    pub sensitivity_db: f32,   // -20 dB to +40 dB input stage gain
    pub fast_peak_meter: bool, // false = Slow RMS, true = Fast Peak
    pub audio_monitor: f32,    // 0.0 to 1.0 output level to main mix
}

impl Default for Bk2107Params {
    fn default() -> Self {
        Self {
            enabled: true,
            center_freq_hz: 440.0,
            q_factor: 35.0,
            sensitivity_db: 12.0,
            fast_peak_meter: false,
            audio_monitor: 0.7,
        }
    }
}

pub struct Bk2107Analyzer {
    pub params: Bk2107Params,
    svf_stage1: SvfFilter,
    svf_stage2: SvfFilter,
    pub meter: BallisticMeter,
    pub overload: bool,
}

impl Default for Bk2107Analyzer {
    fn default() -> Self {
        Self {
            params: Bk2107Params::default(),
            svf_stage1: SvfFilter::default(),
            svf_stage2: SvfFilter::default(),
            meter: BallisticMeter::default(),
            overload: false,
        }
    }
}

impl Bk2107Analyzer {
    pub fn process(&mut self, input: f32, cv_mod: f32, supply_sag: f32, sample_rate: f32) -> f32 {
        if !self.params.enabled {
            self.meter.process(0.0, self.params.fast_peak_meter, sample_rate);
            self.overload = false;
            return 0.0;
        }

        // Input sensitivity stage with discrete soft-clipping
        let gain = 10.0_f32.powf(clamp(self.params.sensitivity_db, -20.0, 40.0) / 20.0);
        let pre = input * gain;
        self.overload = pre.abs() > 1.15 * supply_sag;
        let clipped = soft_clip(pre, 1.3 / supply_sag.max(0.4));

        // Ultra-narrow variable-Q Butterworth resonant bandpass structure
        let mod_freq = clamp(
            self.params.center_freq_hz * 2.0_f32.powf(cv_mod),
            20.0,
            20_000.0,
        );
        let q = clamp(self.params.q_factor, 1.0, 140.0);
        // Cascade 2 resonant bandpass stages with stagger/Q distribution for true B&K 2107 selectivity
        let stage_q = (q * 0.7071).max(0.7071);
        self.svf_stage1.set_params(mod_freq, stage_q, sample_rate);
        self.svf_stage2.set_params(mod_freq, stage_q, sample_rate);

        let (_, bp1, _, _) = self.svf_stage1.process_multimode(clipped);
        let (_, bp2, _, _) = self.svf_stage2.process_multimode(bp1);

        // Makeup resonant gain for narrow Q musicality ("pinging" the analyzer)
        let out = soft_clip(bp2 * (1.0 + (q / 45.0).min(2.2)), 1.0);
        self.meter
            .process(out, self.params.fast_peak_meter, sample_rate);
        out * self.params.audio_monitor
    }
}

// ============================================================================
// 3. EG&G PRINCETON APPLIED RESEARCH MODEL 124A LOCK-IN AMPLIFIER
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Par124aCoupling {
    Direct,
    Transformer,
}

impl Par124aCoupling {
    pub fn toggle(self) -> Self {
        match self {
            Self::Direct => Self::Transformer,
            Self::Transformer => Self::Direct,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Par124aFilterMode {
    Flat,
    Bandpass,
    Notch,
    LowPass,
    HighPass,
}

impl Par124aFilterMode {
    pub fn next(self) -> Self {
        match self {
            Self::Flat => Self::Bandpass,
            Self::Bandpass => Self::Notch,
            Self::Notch => Self::LowPass,
            Self::LowPass => Self::HighPass,
            Self::HighPass => Self::Flat,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "FLAT",
            Self::Bandpass => "BANDPASS",
            Self::Notch => "NOTCH (-80dB)",
            Self::LowPass => "LOW PASS (12dB)",
            Self::HighPass => "HIGH PASS",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Par124aDynamicReserve {
    LoDrift,
    Normal,
    HiDynamicReserve,
}

impl Par124aDynamicReserve {
    pub fn next(self) -> Self {
        match self {
            Self::LoDrift => Self::Normal,
            Self::Normal => Self::HiDynamicReserve,
            Self::HiDynamicReserve => Self::LoDrift,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LoDrift => "LO DRIFT",
            Self::Normal => "NORMAL",
            Self::HiDynamicReserve => "HI DYN RES (1000x)",
        }
    }
}

/// Time constants from 1 ms to 300 s (12 positions on PAR 124A front panel)
pub const PAR124A_TIME_CONSTANTS: [f32; 12] = [
    0.001, 0.003, 0.010, 0.030, 0.100, 0.300, 1.0, 3.0, 10.0, 30.0, 100.0, 300.0,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Par124aParams {
    pub enabled: bool,
    pub coupling: Par124aCoupling,
    pub filter_mode: Par124aFilterMode,
    pub filter_freq_hz: f32,
    pub filter_q: f32, // 1.0 to 100.0
    pub external_ref: bool,
    pub internal_ref_hz: f32,
    pub phase_quadrant: u8,    // 0, 1 (90°), 2 (180°), 3 (270°)
    pub phase_vernier_deg: f32, // 0.0 to 100.0° (10-turn continuous dial)
    pub tc_index: usize,        // 0..11 => 1ms to 300s
    pub slope_12db: bool,       // false = 6 dB/oct, true = 12 dB/oct
    pub dynamic_reserve: Par124aDynamicReserve,
    pub sensitivity_gain: f32,  // 0.2 to 10.0
    pub audio_mix_level: f32,   // 0.0 to 1.0 (blends PSD heterodyne & demod output into audio bus)
}

impl Default for Par124aParams {
    fn default() -> Self {
        Self {
            enabled: true,
            coupling: Par124aCoupling::Transformer,
            filter_mode: Par124aFilterMode::Bandpass,
            filter_freq_hz: 220.0,
            filter_q: 15.0,
            external_ref: false,
            internal_ref_hz: 223.0, // Slight offset from 220Hz creates rhythmic lock-in beating
            phase_quadrant: 0,
            phase_vernier_deg: 15.0,
            tc_index: 2, // 10 ms default for responsive audio/meter action
            slope_12db: true,
            dynamic_reserve: Par124aDynamicReserve::Normal,
            sensitivity_gain: 2.0,
            audio_mix_level: 0.55,
        }
    }
}

impl Par124aParams {
    pub fn total_phase_deg(&self) -> f32 {
        (self.phase_quadrant as f32) * 90.0 + clamp(self.phase_vernier_deg, 0.0, 100.0)
    }

    pub fn time_constant_sec(&self) -> f32 {
        PAR124A_TIME_CONSTANTS[self.tc_index.min(PAR124A_TIME_CONSTANTS.len() - 1)]
    }
}

pub struct Par124aLockIn {
    pub params: Par124aParams,
    sig_svf1: SvfFilter,
    sig_svf2: SvfFilter,
    sig_svf3: SvfFilter,
    xfmr_hp: OnePoleFilter,
    out_lp1: OnePoleFilter,
    out_lp2: OnePoleFilter,
    ref_phase: f64,
    rng: FastRng,
    pink: PinkNoiseFilter,
    pub pre_overload: bool,
    pub post_overload: bool,
    pub demod_meter: f32,
}

impl Default for Par124aLockIn {
    fn default() -> Self {
        Self {
            params: Par124aParams::default(),
            sig_svf1: SvfFilter::default(),
            sig_svf2: SvfFilter::default(),
            sig_svf3: SvfFilter::default(),
            xfmr_hp: OnePoleFilter::default(),
            out_lp1: OnePoleFilter::default(),
            out_lp2: OnePoleFilter::default(),
            ref_phase: 0.0,
            rng: FastRng::new(0x124A_E660),
            pink: PinkNoiseFilter::default(),
            pre_overload: false,
            post_overload: false,
            demod_meter: 0.0,
        }
    }
}

impl Par124aLockIn {
    pub fn process(
        &mut self,
        input: f32,
        ext_ref_phase: f32,
        cv_mod: f32,
        supply_sag: f32,
        sample_rate: f32,
    ) -> f32 {
        if !self.params.enabled {
            self.pre_overload = false;
            self.post_overload = false;
            self.demod_meter = 0.0;
            return 0.0;
        }

        // 1. Input stage: Direct vs Transformer coupling with simulated noise floor contours
        let white = self.rng.white_noise();
        let flicker = self.pink.process(white);
        let coupled_in = match self.params.coupling {
            Par124aCoupling::Direct => {
                // Direct FET input: passes DC, slightly higher 1/f flicker noise floor
                input + white * 0.0008 + flicker * 0.0022
            }
            Par124aCoupling::Transformer => {
                // Transformer coupling: blocks DC (< 12 Hz), steps up voltage (+6dB), ultra-low thermal noise
                self.xfmr_hp.set_cutoff(12.0, sample_rate);
                let ac = self.xfmr_hp.process_hp(input);
                transformer_saturate(ac * 1.4, supply_sag) + white * 0.00025
            }
        };

        // Dynamic Reserve distribution between Pre-PSD AC gain and Post-PSD DC gain
        let (pre_gain, post_gain, reserve_headroom): (f32, f32, f32) =
            match self.params.dynamic_reserve {
                Par124aDynamicReserve::LoDrift => (self.params.sensitivity_gain * 2.5, 0.8, 1.0),
                Par124aDynamicReserve::Normal => (self.params.sensitivity_gain * 1.0, 2.0, 10.0),
                Par124aDynamicReserve::HiDynamicReserve => {
                    // Up to 1000x (60 dB) dynamic reserve ahead of overload
                    (self.params.sensitivity_gain * 0.12, 16.5, 1000.0)
                }
            };

        let pre_sig = coupled_in * pre_gain;
        self.pre_overload = pre_sig.abs() > (1.5 * supply_sag * (reserve_headroom.ln().max(1.0)));

        // 2. Signal Channel Filter: FLAT, BANDPASS (Q 1..100), NOTCH (80 dB), LOW PASS (12dB/oct), HIGH PASS
        let fc = clamp(
            self.params.filter_freq_hz * 2.0_f32.powf(cv_mod * 0.5),
            5.0,
            20_000.0,
        );
        let q = clamp(self.params.filter_q, 1.0, 100.0);

        let filtered_sig = match self.params.filter_mode {
            Par124aFilterMode::Flat => pre_sig,
            Par124aFilterMode::Bandpass => {
                self.sig_svf1.set_params(fc, q, sample_rate);
                let (_, bp, _, _) = self.sig_svf1.process_multimode(pre_sig);
                bp * (1.0 + (q / 40.0).min(1.5))
            }
            Par124aFilterMode::Notch => {
                // 3-stage cascaded high-precision notch achieving >= 80 dB attenuation at center freq
                self.sig_svf1.set_params(fc, 6.0, sample_rate);
                self.sig_svf2.set_params(fc, 6.0, sample_rate);
                self.sig_svf3.set_params(fc, 6.0, sample_rate);
                let (_, _, _, n1) = self.sig_svf1.process_multimode(pre_sig);
                let (_, _, _, n2) = self.sig_svf2.process_multimode(n1);
                let (_, _, _, n3) = self.sig_svf3.process_multimode(n2);
                n3
            }
            Par124aFilterMode::LowPass => {
                // 12 dB/octave Butterworth lowpass (Q = 0.7071)
                self.sig_svf1.set_params(fc, 0.7071, sample_rate);
                let (lp, _, _, _) = self.sig_svf1.process_multimode(pre_sig);
                lp
            }
            Par124aFilterMode::HighPass => {
                self.sig_svf1.set_params(fc, 0.7071, sample_rate);
                let (_, _, hp, _) = self.sig_svf1.process_multimode(pre_sig);
                hp
            }
        };

        // 3. Phase Shifter & Reference Channel
        let ref_freq = clamp(
            self.params.internal_ref_hz * 2.0_f32.powf(cv_mod),
            1.0,
            20_000.0,
        );
        self.ref_phase = (self.ref_phase + (ref_freq as f64) / (sample_rate as f64)).fract();

        let base_ref_phase = if self.params.external_ref {
            ext_ref_phase
        } else {
            self.ref_phase as f32
        };

        let phase_shift_cycles = self.params.total_phase_deg() / 360.0;
        let shifted_phase = (base_ref_phase + phase_shift_cycles).fract();
        // Synchronous switching square reference (+1 / -1) as in authentic PAR 124A PSD
        let psd_ref_sq = if shifted_phase < 0.5 { 1.0 } else { -1.0 };
        let psd_ref_sin = (shifted_phase * TAU).sin();

        // 4. Phase Sensitive Detector (PSD) / Synchronous Mixer
        let psd_product = filtered_sig * psd_ref_sq;

        // 5. Low-Pass Output Filter (1 ms to 300 s, 6 dB/oct or 12 dB/oct)
        let tau = self.params.time_constant_sec();
        self.out_lp1.set_time_constant(tau, sample_rate);
        self.out_lp2.set_time_constant(tau, sample_rate);

        let lp1 = self.out_lp1.process_lp(psd_product);
        let dc_out = if self.params.slope_12db {
            self.out_lp2.process_lp(lp1)
        } else {
            lp1
        } * post_gain;

        self.post_overload = dc_out.abs() > 1.25 * supply_sag;
        self.demod_meter = clamp(dc_out, -1.5, 1.5);

        // For audible sound design (Hainbach Lock-In technique), output combines the
        // PSD synchronous ring-modulated signal + DC envelope modulated reference carrier
        let audible_psd = (psd_product * 0.55 + dc_out * psd_ref_sin * 0.65) * self.params.audio_mix_level;
        soft_clip(audible_psd, 1.0)
    }
}

// ============================================================================
// 4. BRÜEL & KJÆR TYPE 1621 TUNABLE BAND PASS FILTER
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bk1621Bandwidth {
    Pct3,   // 3% narrow constant-percentage bandwidth (Q ≈ 33.33)
    Pct23,  // 23% 1/3-octave ISO R 266 standard (Q ≈ 4.35)
    Linear, // Bypass mode
}

impl Bk1621Bandwidth {
    pub fn next(self) -> Self {
        match self {
            Self::Pct3 => Self::Pct23,
            Self::Pct23 => Self::Linear,
            Self::Linear => Self::Pct3,
        }
    }

    pub fn q_factor(self) -> f32 {
        match self {
            Self::Pct3 => 33.33,
            Self::Pct23 => 4.35,
            Self::Linear => 0.707,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pct3 => "3% (Q=33.3)",
            Self::Pct23 => "23% (1/3 Oct)",
            Self::Linear => "LINEAR",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bk1621Decade {
    D0_2To2,     // 0.2 - 2 Hz
    D2To20,      // 2 - 20 Hz
    D20To200,    // 20 - 200 Hz
    D200To2000,  // 200 - 2000 Hz
    D2kTo20k,    // 2k - 20 kHz
}

impl Bk1621Decade {
    pub fn multiplier(self) -> f32 {
        match self {
            Self::D0_2To2 => 1.0,
            Self::D2To20 => 10.0,
            Self::D20To200 => 100.0,
            Self::D200To2000 => 1_000.0,
            Self::D2kTo20k => 10_000.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::D0_2To2 => "0.2-2 Hz",
            Self::D2To20 => "2-20 Hz",
            Self::D20To200 => "20-200 Hz",
            Self::D200To2000 => "200-2k Hz",
            Self::D2kTo20k => "2k-20k Hz",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::D0_2To2 => Self::D2To20,
            Self::D2To20 => Self::D20To200,
            Self::D20To200 => Self::D200To2000,
            Self::D200To2000 => Self::D2kTo20k,
            Self::D2kTo20k => Self::D0_2To2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bk1621Params {
    pub enabled: bool,
    pub bandwidth: Bk1621Bandwidth,
    pub decade: Bk1621Decade,
    pub dial_tuning: f32,     // 0.20 to 2.00
    pub range_0_2v: bool,     // true = 0.2 V RMS high sensitivity, false = 1.0 V RMS
    pub auto_sweep: bool,     // Automated decade sweep via internal ramp generator
    pub sweep_rate_hz: f32,   // 0.05 to 2.0 Hz ramp sweep speed
    pub output_level: f32,
}

impl Default for Bk1621Params {
    fn default() -> Self {
        Self {
            enabled: true,
            bandwidth: Bk1621Bandwidth::Pct3,
            decade: Bk1621Decade::D200To2000,
            dial_tuning: 0.65, // 650 Hz
            range_0_2v: false,
            auto_sweep: false,
            sweep_rate_hz: 0.20,
            output_level: 0.75,
        }
    }
}

pub struct Bk1621Filter {
    pub params: Bk1621Params,
    svf: SvfFilter,
    sweep_phase: f32,
    pub current_freq_hz: f32,
    pub phase_shift_deg: f32, // Realistic +90° to -90° phase shift indicator
    pub overload_lamp: bool,
}

impl Default for Bk1621Filter {
    fn default() -> Self {
        Self {
            params: Bk1621Params::default(),
            svf: SvfFilter::default(),
            sweep_phase: 0.0,
            current_freq_hz: 650.0,
            phase_shift_deg: 0.0,
            overload_lamp: false,
        }
    }
}

impl Bk1621Filter {
    pub fn process(
        &mut self,
        input: f32,
        dominant_input_freq_hz: f32,
        cv_mod: f32,
        supply_sag: f32,
        sample_rate: f32,
    ) -> f32 {
        if !self.params.enabled {
            self.overload_lamp = false;
            return 0.0;
        }

        // Internal ramp sweep generator (logarithmic across decade 0.2 .. 2.0)
        let dial = if self.params.auto_sweep {
            let rate = clamp(self.params.sweep_rate_hz, 0.02, 5.0);
            self.sweep_phase = (self.sweep_phase + rate / sample_rate).fract();
            0.2 * (10.0_f32).powf(self.sweep_phase)
        } else {
            clamp(self.params.dial_tuning, 0.2, 2.0)
        };

        let base_fc = dial * self.params.decade.multiplier();
        let fc = clamp(base_fc * 2.0_f32.powf(cv_mod), 0.2, 20_000.0);
        self.current_freq_hz = fc;

        // Input voltage range selector: 0.2 V RMS vs 1.0 V RMS
        let max_v = if self.params.range_0_2v { 0.28 } else { 1.41 };
        self.overload_lamp = input.abs() > max_v * supply_sag;
        let scaled_in = if self.params.range_0_2v {
            soft_clip(input * 3.2, 1.5)
        } else {
            soft_clip(input, 1.1)
        };

        // Compute realistic single-pole Butterworth integrator resonant phase shift (+90° to -90°)
        let q = self.params.bandwidth.q_factor();
        let freq_ratio = dominant_input_freq_hz.max(1.0) / fc.max(0.2);
        let tan_phi = -q * (freq_ratio - 1.0 / freq_ratio);
        self.phase_shift_deg = match self.params.bandwidth {
            Bk1621Bandwidth::Linear => 0.0,
            _ => tan_phi.atan().to_degrees(),
        };

        let out = match self.params.bandwidth {
            Bk1621Bandwidth::Linear => scaled_in,
            Bk1621Bandwidth::Pct3 | Bk1621Bandwidth::Pct23 => {
                self.svf.set_params(fc, q, sample_rate);
                let (_, bp, _, _) = self.svf.process_multimode(scaled_in);
                bp * (1.0 + (q / 50.0))
            }
        };

        out * self.params.output_level
    }
}

// ============================================================================
// 5. KOMA ELEKTRONIK STROM MOBILE EMULATION (Power Supply, Sag, Hum & Dual LFO)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LfoShape {
    Sine,
    Triangle,
    Ramp,
    SampleAndHold,
}

impl LfoShape {
    pub fn next(self) -> Self {
        match self {
            Self::Sine => Self::Triangle,
            Self::Triangle => Self::Ramp,
            Self::Ramp => Self::SampleAndHold,
            Self::SampleAndHold => Self::Sine,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sine => "SINE",
            Self::Triangle => "TRI",
            Self::Ramp => "RAMP",
            Self::SampleAndHold => "S&H",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModTarget {
    None,
    Tag101Freq,
    Bk2107Freq,
    Par124aRef,
    Bk1621Freq,
    FuncGenFreq,
    DictaphoneSpeed,
}

impl ModTarget {
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::Tag101Freq,
            Self::Tag101Freq => Self::Bk2107Freq,
            Self::Bk2107Freq => Self::Par124aRef,
            Self::Par124aRef => Self::Bk1621Freq,
            Self::Bk1621Freq => Self::FuncGenFreq,
            Self::FuncGenFreq => Self::DictaphoneSpeed,
            Self::DictaphoneSpeed => Self::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Tag101Freq => "TAG-101 FREQ",
            Self::Bk2107Freq => "B&K 2107 FREQ",
            Self::Par124aRef => "PAR 124A REF",
            Self::Bk1621Freq => "B&K 1621 TUNE",
            Self::FuncGenFreq => "FUNC GEN FREQ",
            Self::DictaphoneSpeed => "TAPE SPEED",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StromMobileParams {
    pub enabled: bool,
    pub lfo1_rate_hz: f32,
    pub lfo1_depth: f32,
    pub lfo1_shape: LfoShape,
    pub lfo1_target: ModTarget,
    pub lfo2_rate_hz: f32,
    pub lfo2_depth: f32,
    pub lfo2_shape: LfoShape,
    pub lfo2_target: ModTarget,
    pub supply_voltage: f32, // 3.0 V (heavy sag) to 9.0 V (nominal)
    pub mains_60hz: bool,    // false = 50 Hz EU, true = 60 Hz US
    pub ripple_hum: f32,     // 0.0 to 1.0 mains hum & ripple injection
}

impl Default for StromMobileParams {
    fn default() -> Self {
        Self {
            enabled: true,
            lfo1_rate_hz: 0.45,
            lfo1_depth: 0.25,
            lfo1_shape: LfoShape::Sine,
            lfo1_target: ModTarget::Bk2107Freq,
            lfo2_rate_hz: 2.1,
            lfo2_depth: 0.15,
            lfo2_shape: LfoShape::SampleAndHold,
            lfo2_target: ModTarget::Bk1621Freq,
            supply_voltage: 8.6,
            mains_60hz: false,
            ripple_hum: 0.04,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StromModOutputs {
    pub lfo1_val: f32,
    pub lfo2_val: f32,
    pub supply_sag_factor: f32,
    pub hum_sample: f32,
}

impl StromModOutputs {
    pub fn cv_for(&self, target: ModTarget, p: &StromMobileParams) -> f32 {
        if !p.enabled {
            return 0.0;
        }
        let mut sum = 0.0;
        if p.lfo1_target == target {
            sum += self.lfo1_val;
        }
        if p.lfo2_target == target {
            sum += self.lfo2_val;
        }
        sum
    }
}

pub struct StromMobile {
    pub params: StromMobileParams,
    phase1: f32,
    phase2: f32,
    sh1_val: f32,
    sh2_val: f32,
    hum_phase: f32,
    rng: FastRng,
}

impl Default for StromMobile {
    fn default() -> Self {
        Self {
            params: StromMobileParams::default(),
            phase1: 0.0,
            phase2: 0.0,
            sh1_val: 0.0,
            sh2_val: 0.0,
            hum_phase: 0.0,
            rng: FastRng::new(0x5720_4B4F),
        }
    }
}

impl StromMobile {
    pub fn process(&mut self, sample_rate: f32) -> StromModOutputs {
        if !self.params.enabled {
            return StromModOutputs {
                lfo1_val: 0.0,
                lfo2_val: 0.0,
                supply_sag_factor: 1.0,
                hum_sample: 0.0,
            };
        }
        let dt = 1.0 / sample_rate;

        // LFO 1
        let r1 = clamp(self.params.lfo1_rate_hz, 0.02, 50.0);
        let prev1 = self.phase1;
        self.phase1 = (self.phase1 + r1 * dt).fract();
        if self.phase1 < prev1 {
            self.sh1_val = self.rng.white_noise();
        }
        let w1 = eval_lfo(self.phase1, self.params.lfo1_shape, self.sh1_val);

        // LFO 2
        let r2 = clamp(self.params.lfo2_rate_hz, 0.02, 50.0);
        let prev2 = self.phase2;
        self.phase2 = (self.phase2 + r2 * dt).fract();
        if self.phase2 < prev2 {
            self.sh2_val = self.rng.white_noise();
        }
        let w2 = eval_lfo(self.phase2, self.params.lfo2_shape, self.sh2_val);

        // Mains hum & rectifier ripple (50/60 Hz fundamental + 100/120 Hz full-wave rectifier ripple + 3rd harmonic)
        let mains_hz = if self.params.mains_60hz { 60.0 } else { 50.0 };
        self.hum_phase = (self.hum_phase + mains_hz * dt).fract();
        let hp = self.hum_phase * TAU;
        let hum = (hp.sin() * 0.65 + (hp * 2.0).sin() * 0.25 + (hp * 3.0).sin() * 0.10)
            * clamp(self.params.ripple_hum, 0.0, 1.0)
            * 0.18;

        let sag = clamp(self.params.supply_voltage / 9.0, 0.30, 1.0);

        StromModOutputs {
            lfo1_val: w1 * clamp(self.params.lfo1_depth, 0.0, 1.0),
            lfo2_val: w2 * clamp(self.params.lfo2_depth, 0.0, 1.0),
            supply_sag_factor: sag,
            hum_sample: hum,
        }
    }
}

#[inline]
fn eval_lfo(phase: f32, shape: LfoShape, sh_val: f32) -> f32 {
    match shape {
        LfoShape::Sine => (phase * TAU).sin(),
        LfoShape::Triangle => {
            if phase < 0.5 {
                phase * 4.0 - 1.0
            } else {
                3.0 - phase * 4.0
            }
        }
        LfoShape::Ramp => phase * 2.0 - 1.0,
        LfoShape::SampleAndHold => sh_val,
    }
}

// ============================================================================
// 6. GENERAL PURPOSE LABORATORY FUNCTION GENERATOR
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FuncGenWave {
    Sine,
    Triangle,
    SquarePwm,
    Sawtooth,
    Ramp,
    WhiteNoise,
    PinkNoise,
}

impl FuncGenWave {
    pub fn next(self) -> Self {
        match self {
            Self::Sine => Self::Triangle,
            Self::Triangle => Self::SquarePwm,
            Self::SquarePwm => Self::Sawtooth,
            Self::Sawtooth => Self::Ramp,
            Self::Ramp => Self::WhiteNoise,
            Self::WhiteNoise => Self::PinkNoise,
            Self::PinkNoise => Self::Sine,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sine => "SINE",
            Self::Triangle => "TRIANGLE",
            Self::SquarePwm => "SQUARE (PWM)",
            Self::Sawtooth => "SAWTOOTH",
            Self::Ramp => "RAMP",
            Self::WhiteNoise => "WHITE NOISE",
            Self::PinkNoise => "PINK NOISE",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabFunctionGenParams {
    pub enabled: bool,
    pub waveform: FuncGenWave,
    pub freq_hz: f32,
    pub pwm_duty: f32,      // 0.05 to 0.95
    pub fm_depth: f32,      // 0.0 to 1.0
    pub fm_log_mode: bool,  // false = Linear FM, true = Logarithmic (V/Oct) FM
    pub am_depth: f32,      // 0.0 to 1.0
    pub dc_offset: f32,     // -0.5 to +0.5 V
    pub level: f32,         // 0.0 to 1.0
}

impl Default for LabFunctionGenParams {
    fn default() -> Self {
        Self {
            enabled: true,
            waveform: FuncGenWave::Triangle,
            freq_hz: 110.0,
            pwm_duty: 0.50,
            fm_depth: 0.15,
            fm_log_mode: true,
            am_depth: 0.10,
            dc_offset: 0.0,
            level: 0.45,
        }
    }
}

pub struct LabFunctionGen {
    pub params: LabFunctionGenParams,
    phase: f32,
    rng: FastRng,
    pink: PinkNoiseFilter,
}

impl Default for LabFunctionGen {
    fn default() -> Self {
        Self {
            params: LabFunctionGenParams::default(),
            phase: 0.0,
            rng: FastRng::new(0xF09C_6E91),
            pink: PinkNoiseFilter::default(),
        }
    }
}

impl LabFunctionGen {
    pub fn process(&mut self, mod_signal: f32, cv_mod: f32, sample_rate: f32) -> f32 {
        if !self.params.enabled {
            return 0.0;
        }

        let base = clamp(self.params.freq_hz, 0.5, 20_000.0);
        let total_mod = mod_signal * self.params.fm_depth + cv_mod;
        let inst_freq = if self.params.fm_log_mode {
            clamp(base * (2.0_f32).powf(total_mod * 2.0), 0.5, 20_000.0)
        } else {
            clamp(base + total_mod * base * 0.9, 0.5, 20_000.0)
        };

        let dt = inst_freq / sample_rate;
        self.phase = (self.phase + dt).fract();
        let p = self.phase;

        let raw = match self.params.waveform {
            FuncGenWave::Sine => (p * TAU).sin(),
            FuncGenWave::Triangle => {
                if p < 0.5 {
                    p * 4.0 - 1.0
                } else {
                    3.0 - p * 4.0
                }
            }
            FuncGenWave::SquarePwm => {
                let duty = clamp(self.params.pwm_duty, 0.05, 0.95);
                let mut sq = if p < duty { 1.0 } else { -1.0 };
                sq += poly_blep(p, dt);
                sq -= poly_blep((p + 1.0 - duty).fract(), dt);
                sq
            }
            FuncGenWave::Sawtooth => {
                let mut saw = 1.0 - 2.0 * p;
                saw -= poly_blep(p, dt);
                saw
            }
            FuncGenWave::Ramp => {
                let mut ramp = 2.0 * p - 1.0;
                ramp += poly_blep(p, dt);
                ramp
            }
            FuncGenWave::WhiteNoise => self.rng.white_noise(),
            FuncGenWave::PinkNoise => {
                let w = self.rng.white_noise();
                self.pink.process(w)
            }
        };

        // Amplitude Modulation (AM) + DC Offset Bias
        let am_mod = 1.0 + mod_signal * clamp(self.params.am_depth, 0.0, 1.0);
        let biased = raw * am_mod * self.params.level + clamp(self.params.dc_offset, -0.5, 0.5);
        clamp(biased, -1.5, 1.5)
    }
}

// ============================================================================
// 7. VISUAL CATHODE-RAY OSCILLOSCOPE
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerMode {
    Auto,
    Normal,
    RisingEdge,
    FallingEdge,
}

impl TriggerMode {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Normal,
            Self::Normal => Self::RisingEdge,
            Self::RisingEdge => Self::FallingEdge,
            Self::FallingEdge => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "AUTO",
            Self::Normal => "NORMAL",
            Self::RisingEdge => "EDGE RISING",
            Self::FallingEdge => "EDGE FALLING",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhosphorColor {
    GreenP31,
    AmberP7,
}

impl PhosphorColor {
    pub fn toggle(self) -> Self {
        match self {
            Self::GreenP31 => Self::AmberP7,
            Self::AmberP7 => Self::GreenP31,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrtOscilloscopeParams {
    pub timebase_ms_div: f32, // 0.2 to 50.0 ms/div
    pub volts_per_div: f32,   // 0.1 to 5.0 V/div
    pub trigger_mode: TriggerMode,
    pub trigger_threshold: f32, // -1.0 to 1.0 V
    pub phosphor: PhosphorColor,
    pub persistence: f32, // 0.1 to 0.95
}

impl Default for CrtOscilloscopeParams {
    fn default() -> Self {
        Self {
            timebase_ms_div: 2.5,
            volts_per_div: 0.5,
            trigger_mode: TriggerMode::RisingEdge,
            trigger_threshold: 0.05,
            phosphor: PhosphorColor::GreenP31,
            persistence: 0.82,
        }
    }
}

// ============================================================================
// 8. LO-FI TAPE MICRO-SAMPLER / DICTAPHONE
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TapeDictaphoneParams {
    pub enabled: bool,
    pub playing: bool,
    pub reverse: bool,
    pub speed_octaves: f32, // -2.0 to +2.0 octaves varispeed
    pub wow_flutter: f32,   // 0.0 to 1.0
    pub tape_hiss: f32,     // 0.0 to 1.0
    pub head_drive: f32,    // 0.0 to 1.0 saturation compression
    pub dictaphone_bandlimit: bool, // 300 Hz - 3.8 kHz micro-cassette filter
    pub level: f32,
    pub active_preset: usize,
    pub loaded_name: String,
}

impl Default for TapeDictaphoneParams {
    fn default() -> Self {
        Self {
            enabled: true,
            playing: true,
            reverse: false,
            speed_octaves: 0.0,
            wow_flutter: 0.35,
            tape_hiss: 0.18,
            head_drive: 0.45,
            dictaphone_bandlimit: true,
            level: 0.55,
            active_preset: 0,
            loaded_name: "Lab Telemetry & Tape Chords (Internal)".to_string(),
        }
    }
}

pub struct TapeDictaphone {
    pub params: TapeDictaphoneParams,
    sample_buffer: Vec<f32>,
    playhead: f64,
    wow_phase: f32,
    flutter_phase: f32,
    jitter_lp: OnePoleFilter,
    hp_300: SvfFilter,
    lp_3800: SvfFilter,
    comp_env: f32,
    rng: FastRng,
    pink: PinkNoiseFilter,
}

impl Default for TapeDictaphone {
    fn default() -> Self {
        let mut d = Self {
            params: TapeDictaphoneParams::default(),
            sample_buffer: Vec::new(),
            playhead: 0.0,
            wow_phase: 0.0,
            flutter_phase: 0.0,
            jitter_lp: OnePoleFilter::default(),
            hp_300: SvfFilter::default(),
            lp_3800: SvfFilter::default(),
            comp_env: 0.2,
            rng: FastRng::new(0xD1C7_A90E),
            pink: PinkNoiseFilter::default(),
        };
        d.generate_builtin_tape_loop(0, 48000);
        d
    }
}

impl TapeDictaphone {
    /// Generates evocative vintage test-equipment / Hainbach tape loop presets out of the box
    pub fn generate_builtin_tape_loop(&mut self, preset_idx: usize, sample_rate: u32) {
        let sr = sample_rate.max(8000) as usize;
        let len = sr * 6; // 6-second loop
        let mut buf = vec![0.0_f32; len];

        match preset_idx % 3 {
            0 => {
                // Preset 0: Vintage Lab Telemetry Plucks & Warm Organ Fifths
                self.params.loaded_name = "01: Radiophonic Telemetry & Organ Loop".to_string();
                let notes = [110.0_f32, 164.81, 220.0, 261.63, 329.63, 196.0];
                for (i, sample) in buf.iter_mut().enumerate() {
                    let t = (i as f32) / (sr as f32);
                    let step = ((t * 2.0) as usize) % notes.len();
                    let local_t = (t * 2.0).fract();
                    let env = (-local_t * 4.5).exp();
                    let f = notes[step];
                    let pluck = ((t * f * TAU).sin() + 0.4 * (t * f * 2.0 * TAU).sin()) * env;
                    let drone = ((t * 110.0 * TAU).sin() + 0.5 * (t * 165.0 * TAU).sin()) * 0.25;
                    *sample = (pluck * 0.65 + drone) * 0.7;
                }
            }
            1 => {
                // Preset 1: Numbers Station Morse & Sub-Bass Pulses
                self.params.loaded_name = "02: Shortwave Morse Beacon & Sub Pulse".to_string();
                for (i, sample) in buf.iter_mut().enumerate() {
                    let t = (i as f32) / (sr as f32);
                    let gate = if ((t * 8.0) as usize) % 3 != 0 && ((t * 16.0) as usize) % 2 == 0 {
                        1.0
                    } else {
                        0.0
                    };
                    let beep = (t * 780.0 * TAU).sin() * gate * 0.45;
                    let sub_env = (-((t * 1.5).fract()) * 6.0).exp();
                    let sub = (t * 82.4 * TAU).sin() * sub_env * 0.55;
                    *sample = beep + sub;
                }
            }
            _ => {
                // Preset 2: Melancholic Mellotron/Harmonium Tape Chord
                self.params.loaded_name = "03: Worn Micro-Cassette Harmonium Chord".to_string();
                let chord = [146.83_f32, 220.0, 261.63, 349.23, 440.0];
                for (i, sample) in buf.iter_mut().enumerate() {
                    let t = (i as f32) / (sr as f32);
                    let swell = 0.55 + 0.45 * (t * PI / 3.0).sin();
                    let mut acc = 0.0;
                    for (k, &f) in chord.iter().enumerate() {
                        let detune = 1.0 + (k as f32 - 2.0) * 0.0025 * (t * 1.7).sin();
                        acc += (t * f * detune * TAU).sin() * 0.22;
                        acc += (t * f * 2.0 * detune * TAU).sin() * 0.08;
                    }
                    *sample = acc * swell;
                }
            }
        }

        self.sample_buffer = buf;
        self.playhead = 0.0;
        self.params.active_preset = preset_idx % 3;
    }

    pub fn set_custom_buffer(&mut self, samples: Vec<f32>, name: String) {
        if !samples.is_empty() {
            self.sample_buffer = samples;
            self.playhead = 0.0;
            self.params.loaded_name = name;
        }
    }

    pub fn playhead_norm(&self) -> f32 {
        if self.sample_buffer.is_empty() {
            0.0
        } else {
            (self.playhead as f32 / self.sample_buffer.len() as f32).clamp(0.0, 1.0)
        }
    }

    pub fn process(&mut self, cv_mod: f32, supply_sag: f32, sample_rate: f32) -> f32 {
        if !self.params.enabled || !self.params.playing || self.sample_buffer.is_empty() {
            return 0.0;
        }

        let dt = 1.0 / sample_rate;
        let wf = clamp(self.params.wow_flutter, 0.0, 1.0);

        // Dual sinusoidal wobble (1.4 Hz capstan wow + 7.2 Hz pinch-roller flutter + filtered stochastic jitter)
        self.wow_phase = (self.wow_phase + 1.4 * dt).fract();
        self.flutter_phase = (self.flutter_phase + 7.2 * dt).fract();
        self.jitter_lp.set_cutoff(14.0, sample_rate);
        let jitter = self.jitter_lp.process_lp(self.rng.white_noise());

        let pitch_wobble = wf
            * (0.018 * (self.wow_phase * TAU).sin()
                + 0.006 * (self.flutter_phase * TAU).sin()
                + 0.008 * jitter);

        let oct = clamp(self.params.speed_octaves + cv_mod, -2.0, 2.0);
        // Supply sag slows down DC cassette motor slightly
        let motor_sag = 0.92 + 0.08 * supply_sag;
        let speed = (2.0_f32).powf(oct) * (1.0 + pitch_wobble) * motor_sag;
        let signed_speed = if self.params.reverse {
            -(speed as f64)
        } else {
            speed as f64
        };

        let len = self.sample_buffer.len();
        self.playhead += signed_speed;
        while self.playhead >= len as f64 {
            self.playhead -= len as f64;
        }
        while self.playhead < 0.0 {
            self.playhead += len as f64;
        }

        let idx0 = (self.playhead as usize) % len;
        let idx1 = (idx0 + 1) % len;
        let frac = (self.playhead - (idx0 as f64)) as f32;
        let raw_sample = self.sample_buffer[idx0] + frac * (self.sample_buffer[idx1] - self.sample_buffer[idx0]);

        // Add micro-cassette tape hiss (pink + high-frequency white oxide noise)
        let w = self.rng.white_noise();
        let p = self.pink.process(w);
        let hiss = (p * 0.7 + w * 0.3) * clamp(self.params.tape_hiss, 0.0, 1.0) * 0.06;

        let with_hiss = raw_sample + hiss;

        // Head saturation compression (automatic gain control + magnetic oxide soft saturation)
        let abs_s = with_hiss.abs();
        let alpha = if abs_s > self.comp_env { 0.015 } else { 0.0005 };
        self.comp_env += alpha * (abs_s - self.comp_env);
        let agc_gain = 1.0 / (0.35 + self.comp_env * clamp(self.params.head_drive, 0.0, 1.0) * 1.8);
        let drive = 1.0 + clamp(self.params.head_drive, 0.0, 1.0) * 3.5;
        let saturated = soft_clip(with_hiss * agc_gain, drive);

        // Dictaphone band-limiting bandpass filter (300 Hz - 3.8 kHz)
        let filtered = if self.params.dictaphone_bandlimit {
            self.hp_300.set_params(300.0, 0.85, sample_rate);
            self.lp_3800.set_params(3800.0, 0.95, sample_rate);
            let (_, _, hp, _) = self.hp_300.process_multimode(saturated);
            let (lp, _, _, _) = self.lp_3800.process_multimode(hp);
            lp
        } else {
            saturated
        };

        filtered * self.params.level
    }
}
