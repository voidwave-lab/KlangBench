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
use std::f32::consts::PI;

use super::{clamp, BallisticMeter, OnePoleFilter};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackInputSource {
    Tag101,
    FuncGen,
    Dictaphone,
    Bk2107,
    Bk1621,
    Par124a,
    PedalBus,
}

impl TrackInputSource {
    pub fn next(self) -> Self {
        match self {
            Self::Tag101 => Self::FuncGen,
            Self::FuncGen => Self::Dictaphone,
            Self::Dictaphone => Self::Bk2107,
            Self::Bk2107 => Self::Bk1621,
            Self::Bk1621 => Self::Par124a,
            Self::Par124a => Self::PedalBus,
            Self::PedalBus => Self::Tag101,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Tag101 => "TAG-101 OSC",
            Self::FuncGen => "FUNC GEN",
            Self::Dictaphone => "DICTAPHONE",
            Self::Bk2107 => "B&K 2107",
            Self::Bk1621 => "B&K 1621",
            Self::Par124a => "PAR 124A",
            Self::PedalBus => "PEDAL BUS",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportMode {
    Stop,
    Play,
    FastForward,
    Rewind,
}

impl TransportMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stop => "STOP",
            Self::Play => "PLAY",
            Self::FastForward => "FFWD >>",
            Self::Rewind => "<< REW",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TascamChannelParams {
    pub rec_arm: bool,
    pub input_source: TrackInputSource,
    pub level_trim: f32, // 0.0 to 1.5
    pub pan: f32,        // -1.0 (L) to +1.0 (R)
    pub mute: bool,
    pub solo: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TascamState {
    pub transport: TransportMode,
    pub varispeed_pct: f32, // -15.0% to +15.0%
    pub tape_drive: f32,    // 1.0 to 5.0 (k in tanh(k * x))
    pub crosstalk_amt: f32, // 0.0 to 0.25 inter-track bleed
    pub monitor_tape: bool,
    pub channels: [TascamChannelParams; 4],
}

impl Default for TascamState {
    fn default() -> Self {
        Self {
            transport: TransportMode::Stop,
            varispeed_pct: 0.0,
            tape_drive: 2.2,
            crosstalk_amt: 0.045,
            monitor_tape: true,
            channels: [
                TascamChannelParams {
                    rec_arm: false,
                    input_source: TrackInputSource::Tag101,
                    level_trim: 0.85,
                    pan: -0.65,
                    mute: false,
                    solo: false,
                },
                TascamChannelParams {
                    rec_arm: false,
                    input_source: TrackInputSource::Bk2107,
                    level_trim: 0.80,
                    pan: 0.65,
                    mute: false,
                    solo: false,
                },
                TascamChannelParams {
                    rec_arm: false,
                    input_source: TrackInputSource::Dictaphone,
                    level_trim: 0.75,
                    pan: -0.25,
                    mute: false,
                    solo: false,
                },
                TascamChannelParams {
                    rec_arm: false,
                    input_source: TrackInputSource::Par124a,
                    level_trim: 0.75,
                    pan: 0.25,
                    mute: false,
                    solo: false,
                },
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SourceSignals {
    pub tag101: f32,
    pub func_gen: f32,
    pub dictaphone: f32,
    pub bk2107: f32,
    pub bk1621: f32,
    pub par124a: f32,
    pub pedal_bus: f32,
}

impl SourceSignals {
    pub fn pick(&self, src: TrackInputSource) -> f32 {
        match src {
            TrackInputSource::Tag101 => self.tag101,
            TrackInputSource::FuncGen => self.func_gen,
            TrackInputSource::Dictaphone => self.dictaphone,
            TrackInputSource::Bk2107 => self.bk2107,
            TrackInputSource::Bk1621 => self.bk1621,
            TrackInputSource::Par124a => self.par124a,
            TrackInputSource::PedalBus => self.pedal_bus,
        }
    }
}

pub struct Tascam4Track {
    pub state: TascamState,
    tracks: [Vec<f32>; 4],
    tape_pos: f64,
    hf_filters: [OnePoleFilter; 4],
    pub meters: [BallisticMeter; 4],
}

impl Default for Tascam4Track {
    fn default() -> Self {
        let loop_len = 48_000 * 12; // 12-second 4-track cassette loop reel
        Self {
            state: TascamState::default(),
            tracks: [
                vec![0.0; loop_len],
                vec![0.0; loop_len],
                vec![0.0; loop_len],
                vec![0.0; loop_len],
            ],
            tape_pos: 0.0,
            hf_filters: [
                OnePoleFilter::default(),
                OnePoleFilter::default(),
                OnePoleFilter::default(),
                OnePoleFilter::default(),
            ],
            meters: [
                BallisticMeter::default(),
                BallisticMeter::default(),
                BallisticMeter::default(),
                BallisticMeter::default(),
            ],
        }
    }
}

impl Tascam4Track {
    /// Optional demo stems generator if user wants pre-populated 4-track tape
    pub fn seed_initial_cassette_tracks(&mut self, _sample_rate: usize) {
        // Start with blank magnetic tape so Tab 3 never plays unexpected clock ticks
        // unless the user records onto tracks 1-4.
        self.clear_all_tracks();
    }

    pub fn clear_all_tracks(&mut self) {
        for trk in &mut self.tracks {
            trk.fill(0.0);
        }
    }

    pub fn tape_counter_seconds(&self, sample_rate: f32) -> f32 {
        (self.tape_pos as f32) / sample_rate.max(1000.0)
    }

    pub fn tape_progress_norm(&self) -> f32 {
        let len = self.tracks[0].len().max(1);
        (self.tape_pos as f32 / len as f32).clamp(0.0, 1.0)
    }

    pub fn process(&mut self, sources: &SourceSignals, sample_rate: f32) -> (f32, f32, [f32; 4]) {
        let len = self.tracks[0].len();
        let varispeed = 1.0 + clamp(self.state.varispeed_pct, -15.0, 15.0) * 0.01;

        let step = match self.state.transport {
            TransportMode::Stop => 0.0,
            TransportMode::Play => varispeed as f64,
            TransportMode::FastForward => (varispeed * 5.5) as f64,
            TransportMode::Rewind => -(varispeed * 5.5) as f64,
        };

        self.tape_pos += step;
        while self.tape_pos >= len as f64 {
            self.tape_pos -= len as f64;
        }
        while self.tape_pos < 0.0 {
            self.tape_pos += len as f64;
        }

        let idx0 = (self.tape_pos as usize) % len;
        let idx1 = (idx0 + 1) % len;
        let frac = (self.tape_pos - idx0 as f64) as f32;

        let k_drive = clamp(self.state.tape_drive, 0.5, 6.0);
        let norm_tanh = k_drive.tanh().max(0.2);

        let mut raw_ch = [0.0_f32; 4];

        for ch in 0..4 {
            let cp = &self.state.channels[ch];
            let src_sig = sources.pick(cp.input_source);

            // If playing and armed for record, write saturated signal onto tape
            if self.state.transport == TransportMode::Play && cp.rec_arm {
                let rec_in = ((src_sig * cp.level_trim * k_drive).tanh()) / norm_tanh;
                // Overdub blend or replace
                self.tracks[ch][idx0] = rec_in;
                raw_ch[ch] = rec_in;
            } else if self.state.transport != TransportMode::Stop {
                let s0 = self.tracks[ch][idx0];
                let s1 = self.tracks[ch][idx1];
                let tape_read = s0 + frac * (s1 - s0);
                // When FFWD/REW, high-speed chipmunk scrub at reduced volume
                let scrub_scale = match self.state.transport {
                    TransportMode::Play => 1.0,
                    _ => 0.35,
                };
                raw_ch[ch] = tape_read * scrub_scale;
            } else {
                // Stopped: monitor live input source only if armed for record
                raw_ch[ch] = if cp.rec_arm { src_sig * 0.5 } else { 0.0 };
            }

            // Tape saturation DSP using soft non-linear transfer function tanh(k * x) + cassette HF smoothing
            self.hf_filters[ch].set_cutoff(11_500.0 * varispeed, sample_rate);
            let sat = ((raw_ch[ch] * cp.level_trim * k_drive).tanh()) / norm_tanh;
            raw_ch[ch] = self.hf_filters[ch].process_lp(sat);
        }

        // Inter-track tape bleed / crosstalk matrix simulation (adjacent tracks 1<->2, 2<->3, 3<->4)
        let xt = clamp(self.state.crosstalk_amt, 0.0, 0.25);
        let xt2 = xt * 0.2;
        let bled = [
            raw_ch[0] + xt * raw_ch[1] + xt2 * raw_ch[2],
            raw_ch[1] + xt * (raw_ch[0] + raw_ch[2]) + xt2 * raw_ch[3],
            raw_ch[2] + xt * (raw_ch[1] + raw_ch[3]) + xt2 * raw_ch[0],
            raw_ch[3] + xt * raw_ch[2] + xt2 * raw_ch[1],
        ];

        let any_solo = self.state.channels.iter().any(|c| c.solo);
        let mut mix_l = 0.0_f32;
        let mut mix_r = 0.0_f32;
        let mut meter_vals = [0.0_f32; 4];

        for ch in 0..4 {
            let cp = &self.state.channels[ch];
            let active = if any_solo {
                cp.solo && !cp.mute
            } else {
                !cp.mute
            };

            let ch_sig = if active { bled[ch] } else { 0.0 };
            meter_vals[ch] = self.meters[ch].process(ch_sig, false, sample_rate);

            // Constant-power stereo pan law
            let pan_norm = (clamp(cp.pan, -1.0, 1.0) + 1.0) * 0.25 * PI;
            mix_l += ch_sig * pan_norm.cos();
            mix_r += ch_sig * pan_norm.sin();
        }

        if !self.state.monitor_tape {
            return (0.0, 0.0, meter_vals);
        }

        (mix_l * 0.65, mix_r * 0.65, meter_vals)
    }
}
