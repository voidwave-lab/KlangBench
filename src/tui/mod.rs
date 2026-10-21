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

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use parking_lot::RwLock;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs};
use ratatui::{Frame, Terminal};

use crossbeam_channel::Sender;

use crate::dsp::clamp;
use crate::dsp::instruments::PAR124A_TIME_CONSTANTS;
use crate::dsp::tascam::TransportMode;
use crate::engine::{EngineCommand, TelemetryFrame};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    LabInstruments = 0,
    Pedalboard = 1,
    TascamRecorder = 2,
}

pub struct TuiApp {
    tab: ActiveTab,
    selected_idx: [usize; 3],
    cmd_tx: Sender<EngineCommand>,
    telemetry: Arc<RwLock<TelemetryFrame>>,
    web_port: u16,
    file_input_mode: bool,
    file_input_buf: String,
}

impl TuiApp {
    pub fn new(
        cmd_tx: Sender<EngineCommand>,
        telemetry: Arc<RwLock<TelemetryFrame>>,
        web_port: u16,
    ) -> Self {
        Self {
            tab: ActiveTab::LabInstruments,
            selected_idx: [0, 0, 0],
            cmd_tx,
            telemetry,
            web_port,
            file_input_mode: false,
            file_input_buf: String::new(),
        }
    }

    fn max_items_for_tab(&self, tab: ActiveTab) -> usize {
        match tab {
            ActiveTab::LabInstruments => 38,
            ActiveTab::Pedalboard => 23,
            ActiveTab::TascamRecorder => 30,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let t = self.tab as usize;
        let max = self.max_items_for_tab(self.tab);
        let cur = self.selected_idx[t] as isize;
        self.selected_idx[t] = (cur + delta).rem_euclid(max as isize) as usize;
    }

    fn adjust_selected(&mut self, dir: f32) {
        let snap = self.telemetry.read().clone();
        let idx = self.selected_idx[self.tab as usize];

        match self.tab {
            ActiveTab::LabInstruments => {
                match idx {
                    // TAG-101 (0..6)
                    0 => {
                        let mut p = snap.tag101;
                        p.enabled = !p.enabled;
                        let _ = self.cmd_tx.send(EngineCommand::SetTag101(p));
                    }
                    1 => {
                        let mut p = snap.tag101;
                        p.dial_freq = clamp(p.dial_freq + dir * 1.5, 10.0, 100.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetTag101(p));
                    }
                    2 => {
                        let mut p = snap.tag101;
                        p.decade = if dir >= 0.0 {
                            p.decade.next()
                        } else {
                            p.decade.prev()
                        };
                        let _ = self.cmd_tx.send(EngineCommand::SetTag101(p));
                    }
                    3 => {
                        let mut p = snap.tag101;
                        p.waveform = p.waveform.toggle();
                        let _ = self.cmd_tx.send(EngineCommand::SetTag101(p));
                    }
                    4 => {
                        let mut p = snap.tag101;
                        let step = if dir >= 0.0 { 10 } else { -10 };
                        p.attenuator_db = (p.attenuator_db + step).clamp(-50, 0);
                        let _ = self.cmd_tx.send(EngineCommand::SetTag101(p));
                    }
                    5 => {
                        let mut p = snap.tag101;
                        p.fine_level = clamp(p.fine_level + dir * 0.05, 0.0, 1.35);
                        let _ = self.cmd_tx.send(EngineCommand::SetTag101(p));
                    }
                    // B&K 2107 (6..11)
                    6 => {
                        let mut p = snap.bk2107;
                        p.enabled = !p.enabled;
                        let _ = self.cmd_tx.send(EngineCommand::SetBk2107(p));
                    }
                    7 => {
                        let mut p = snap.bk2107;
                        p.center_freq_hz =
                            clamp(p.center_freq_hz * (1.0 + dir * 0.06), 20.0, 20_000.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetBk2107(p));
                    }
                    8 => {
                        let mut p = snap.bk2107;
                        p.q_factor = clamp(p.q_factor + dir * 3.0, 1.0, 120.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetBk2107(p));
                    }
                    9 => {
                        let mut p = snap.bk2107;
                        p.sensitivity_db = clamp(p.sensitivity_db + dir * 2.0, -20.0, 40.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetBk2107(p));
                    }
                    10 => {
                        let mut p = snap.bk2107;
                        p.fast_peak_meter = !p.fast_peak_meter;
                        let _ = self.cmd_tx.send(EngineCommand::SetBk2107(p));
                    }
                    // PAR 124A Lock-In (11..21)
                    11 => {
                        let mut p = snap.par124a;
                        p.enabled = !p.enabled;
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    12 => {
                        let mut p = snap.par124a;
                        p.coupling = p.coupling.toggle();
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    13 => {
                        let mut p = snap.par124a;
                        p.filter_mode = p.filter_mode.next();
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    14 => {
                        let mut p = snap.par124a;
                        p.filter_freq_hz =
                            clamp(p.filter_freq_hz * (1.0 + dir * 0.06), 10.0, 20_000.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    15 => {
                        let mut p = snap.par124a;
                        p.filter_q = clamp(p.filter_q + dir * 2.5, 1.0, 100.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    16 => {
                        let mut p = snap.par124a;
                        p.external_ref = !p.external_ref;
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    17 => {
                        let mut p = snap.par124a;
                        p.phase_quadrant = (p.phase_quadrant + 1) % 4;
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    18 => {
                        let mut p = snap.par124a;
                        p.phase_vernier_deg = clamp(p.phase_vernier_deg + dir * 2.5, 0.0, 100.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    19 => {
                        let mut p = snap.par124a;
                        let n = PAR124A_TIME_CONSTANTS.len() as isize;
                        let next = (p.tc_index as isize + if dir >= 0.0 { 1 } else { -1 })
                            .clamp(0, n - 1) as usize;
                        p.tc_index = next;
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    20 => {
                        let mut p = snap.par124a;
                        if dir.abs() > 1.5 {
                            p.slope_12db = !p.slope_12db;
                        } else {
                            p.dynamic_reserve = p.dynamic_reserve.next();
                        }
                        let _ = self.cmd_tx.send(EngineCommand::SetPar124a(p));
                    }
                    // B&K 1621 Filter (21..26)
                    21 => {
                        let mut p = snap.bk1621;
                        p.bandwidth = p.bandwidth.next();
                        let _ = self.cmd_tx.send(EngineCommand::SetBk1621(p));
                    }
                    22 => {
                        let mut p = snap.bk1621;
                        p.decade = p.decade.next();
                        let _ = self.cmd_tx.send(EngineCommand::SetBk1621(p));
                    }
                    23 => {
                        let mut p = snap.bk1621;
                        p.dial_tuning = clamp(p.dial_tuning + dir * 0.05, 0.2, 2.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetBk1621(p));
                    }
                    24 => {
                        let mut p = snap.bk1621;
                        p.range_0_2v = !p.range_0_2v;
                        let _ = self.cmd_tx.send(EngineCommand::SetBk1621(p));
                    }
                    25 => {
                        let mut p = snap.bk1621;
                        p.auto_sweep = !p.auto_sweep;
                        let _ = self.cmd_tx.send(EngineCommand::SetBk1621(p));
                    }
                    // Koma Strom Mobile (26..30)
                    26 => {
                        let mut p = snap.strom;
                        p.lfo1_rate_hz = clamp(p.lfo1_rate_hz + dir * 0.15, 0.05, 30.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetStrom(p));
                    }
                    27 => {
                        let mut p = snap.strom;
                        p.lfo1_shape = p.lfo1_shape.next();
                        let _ = self.cmd_tx.send(EngineCommand::SetStrom(p));
                    }
                    28 => {
                        let mut p = snap.strom;
                        p.supply_voltage = clamp(p.supply_voltage + dir * 0.25, 3.0, 9.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetStrom(p));
                    }
                    29 => {
                        let mut p = snap.strom;
                        p.ripple_hum = clamp(p.ripple_hum + dir * 0.04, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetStrom(p));
                    }
                    // Function Gen (30..33)
                    30 => {
                        let mut p = snap.func_gen;
                        p.waveform = p.waveform.next();
                        let _ = self.cmd_tx.send(EngineCommand::SetFuncGen(p));
                    }
                    31 => {
                        let mut p = snap.func_gen;
                        p.freq_hz = clamp(p.freq_hz * (1.0 + dir * 0.06), 1.0, 15_000.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetFuncGen(p));
                    }
                    32 => {
                        let mut p = snap.func_gen;
                        p.pwm_duty = clamp(p.pwm_duty + dir * 0.04, 0.05, 0.95);
                        let _ = self.cmd_tx.send(EngineCommand::SetFuncGen(p));
                    }
                    // Tape Dictaphone (33..36)
                    33 => {
                        let mut p = snap.dictaphone;
                        p.playing = !p.playing;
                        let _ = self.cmd_tx.send(EngineCommand::SetDictaphone(p));
                    }
                    34 => {
                        let mut p = snap.dictaphone;
                        p.speed_octaves = clamp(p.speed_octaves + dir * 0.1, -2.0, 2.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetDictaphone(p));
                    }
                    35 => {
                        let mut p = snap.dictaphone;
                        if dir < 0.0 {
                            p.reverse = !p.reverse;
                            let _ = self.cmd_tx.send(EngineCommand::SetDictaphone(p));
                        } else {
                            let next_preset = (p.active_preset + 1) % 3;
                            let _ = self
                                .cmd_tx
                                .send(EngineCommand::SelectDictaphonePreset(next_preset));
                        }
                    }
                    // CRT Oscilloscope Controls (36..38)
                    36 => {
                        let mut p = snap.scope;
                        p.timebase_ms_div = clamp(p.timebase_ms_div + dir * 0.4, 0.2, 50.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetScope(p));
                    }
                    _ => {
                        let mut p = snap.scope;
                        if dir.abs() > 1.5 {
                            p.trigger_mode = p.trigger_mode.next();
                        } else {
                            p.volts_per_div = clamp(p.volts_per_div + dir * 0.1, 0.1, 5.0);
                        }
                        let _ = self.cmd_tx.send(EngineCommand::SetScope(p));
                    }
                }
            }
            ActiveTab::Pedalboard => {
                match idx {
                    // Chain order slots 0..3
                    0..=2 => {
                        let _ = self.cmd_tx.send(EngineCommand::SwapPedalSlots {
                            slot_a: idx,
                            slot_b: (idx + 1) % 4,
                        });
                    }
                    // Chorus (3..8)
                    3 => {
                        let mut p = snap.pedalboard.chorus;
                        p.bypass = !p.bypass;
                        let _ = self.cmd_tx.send(EngineCommand::SetChorus(p));
                    }
                    4 => {
                        let mut p = snap.pedalboard.chorus;
                        p.rate_hz = clamp(p.rate_hz + dir * 0.15, 0.1, 8.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetChorus(p));
                    }
                    5 => {
                        let mut p = snap.pedalboard.chorus;
                        p.depth = clamp(p.depth + dir * 0.05, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetChorus(p));
                    }
                    6 => {
                        let mut p = snap.pedalboard.chorus;
                        p.tone = clamp(p.tone + dir * 0.05, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetChorus(p));
                    }
                    7 => {
                        let mut p = snap.pedalboard.chorus;
                        p.wet_dry = clamp(p.wet_dry + dir * 0.05, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetChorus(p));
                    }
                    // Delay (8..13)
                    8 => {
                        let mut p = snap.pedalboard.delay;
                        p.bypass = !p.bypass;
                        let _ = self.cmd_tx.send(EngineCommand::SetDelay(p));
                    }
                    9 => {
                        let mut p = snap.pedalboard.delay;
                        p.delay_ms = clamp(p.delay_ms + dir * 25.0, 10.0, 1500.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetDelay(p));
                    }
                    10 => {
                        let mut p = snap.pedalboard.delay;
                        p.feedback = clamp(p.feedback + dir * 0.04, 0.0, 1.15);
                        let _ = self.cmd_tx.send(EngineCommand::SetDelay(p));
                    }
                    11 => {
                        let mut p = snap.pedalboard.delay;
                        p.ping_pong = !p.ping_pong;
                        let _ = self.cmd_tx.send(EngineCommand::SetDelay(p));
                    }
                    12 => {
                        let mut p = snap.pedalboard.delay;
                        p.mix = clamp(p.mix + dir * 0.05, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetDelay(p));
                    }
                    // Cooper FX Outward MK2 (13..18)
                    13 => {
                        let mut p = snap.pedalboard.outward;
                        p.bypass = !p.bypass;
                        let _ = self.cmd_tx.send(EngineCommand::SetOutward(p));
                    }
                    14 => {
                        let mut p = snap.pedalboard.outward;
                        p.mode = p.mode.next();
                        let _ = self.cmd_tx.send(EngineCommand::SetOutward(p));
                    }
                    15 => {
                        let mut p = snap.pedalboard.outward;
                        p.grain_size_ms = clamp(p.grain_size_ms + dir * 12.0, 5.0, 500.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetOutward(p));
                    }
                    16 => {
                        let mut p = snap.pedalboard.outward;
                        p.pitch_semitones = clamp(p.pitch_semitones + dir * 1.0, -12.0, 12.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetOutward(p));
                    }
                    17 => {
                        let mut p = snap.pedalboard.outward;
                        p.mix = clamp(p.mix + dir * 0.05, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetOutward(p));
                    }
                    // Alesis Wedge Reverb (18..23)
                    18 => {
                        let mut p = snap.pedalboard.reverb;
                        p.bypass = !p.bypass;
                        let _ = self.cmd_tx.send(EngineCommand::SetReverb(p));
                    }
                    19 => {
                        let mut p = snap.pedalboard.reverb;
                        p.room_size = clamp(p.room_size + dir * 0.05, 0.1, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetReverb(p));
                    }
                    20 => {
                        let mut p = snap.pedalboard.reverb;
                        p.decay_time = clamp(p.decay_time + dir * 0.04, 0.1, 0.97);
                        let _ = self.cmd_tx.send(EngineCommand::SetReverb(p));
                    }
                    21 => {
                        let mut p = snap.pedalboard.reverb;
                        p.pre_delay_ms = clamp(p.pre_delay_ms + dir * 5.0, 0.0, 150.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetReverb(p));
                    }
                    _ => {
                        let mut p = snap.pedalboard.reverb;
                        p.mix = clamp(p.mix + dir * 0.05, 0.0, 1.0);
                        let _ = self.cmd_tx.send(EngineCommand::SetReverb(p));
                    }
                }
            }
            ActiveTab::TascamRecorder => {
                let mut ts = snap.tascam;
                if idx < 24 {
                    let ch = idx / 6;
                    let param = idx % 6;
                    match param {
                        0 => ts.channels[ch].rec_arm = !ts.channels[ch].rec_arm,
                        1 => ts.channels[ch].input_source = ts.channels[ch].input_source.next(),
                        2 => {
                            ts.channels[ch].level_trim =
                                clamp(ts.channels[ch].level_trim + dir * 0.05, 0.0, 1.5)
                        }
                        3 => ts.channels[ch].pan = clamp(ts.channels[ch].pan + dir * 0.1, -1.0, 1.0),
                        4 => ts.channels[ch].mute = !ts.channels[ch].mute,
                        _ => ts.channels[ch].solo = !ts.channels[ch].solo,
                    }
                    let _ = self.cmd_tx.send(EngineCommand::SetTascam(ts));
                } else {
                    match idx {
                        24 => {
                            ts.transport = match ts.transport {
                                TransportMode::Play => TransportMode::Stop,
                                _ => TransportMode::Play,
                            };
                            let _ = self.cmd_tx.send(EngineCommand::SetTascam(ts));
                        }
                        25 => {
                            ts.transport = if dir >= 0.0 {
                                TransportMode::FastForward
                            } else {
                                TransportMode::Rewind
                            };
                            let _ = self.cmd_tx.send(EngineCommand::SetTascam(ts));
                        }
                        26 => {
                            ts.varispeed_pct = clamp(ts.varispeed_pct + dir * 1.0, -15.0, 15.0);
                            let _ = self.cmd_tx.send(EngineCommand::SetTascam(ts));
                        }
                        27 => {
                            ts.tape_drive = clamp(ts.tape_drive + dir * 0.2, 0.5, 5.0);
                            let _ = self.cmd_tx.send(EngineCommand::SetTascam(ts));
                        }
                        28 => {
                            ts.crosstalk_amt = clamp(ts.crosstalk_amt + dir * 0.01, 0.0, 0.25);
                            let _ = self.cmd_tx.send(EngineCommand::SetTascam(ts));
                        }
                        _ => {
                            let _ = self.cmd_tx.send(EngineCommand::ToggleMasterRecord);
                        }
                    }
                }
            }
        }
    }
}

pub fn run_tui(
    cmd_tx: Sender<EngineCommand>,
    telemetry: Arc<RwLock<TelemetryFrame>>,
    web_port: u16,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new(cmd_tx, telemetry, web_port);

    let res = run_event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| draw_ui(f, app))?;

        if event::poll(Duration::from_millis(25))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.file_input_mode {
                    match key.code {
                        KeyCode::Esc => {
                            app.file_input_mode = false;
                            app.file_input_buf.clear();
                        }
                        KeyCode::Enter => {
                            let path = app.file_input_buf.trim().to_string();
                            if !path.is_empty() {
                                let _ = app.cmd_tx.send(EngineCommand::LoadDictaphoneFile(path));
                            }
                            app.file_input_mode = false;
                            app.file_input_buf.clear();
                        }
                        KeyCode::Backspace => {
                            app.file_input_buf.pop();
                        }
                        KeyCode::Char(c) => {
                            app.file_input_buf.push(c);
                        }
                        _ => {}
                    }
                    continue;
                }

                // Global quit on Ctrl+C or 'q'
                if key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    break;
                }

                match key.code {
                    KeyCode::Char('1') => app.tab = ActiveTab::LabInstruments,
                    KeyCode::Char('2') => app.tab = ActiveTab::Pedalboard,
                    KeyCode::Char('3') => app.tab = ActiveTab::TascamRecorder,
                    KeyCode::Char(' ') | KeyCode::Char('r') | KeyCode::Char('R') => {
                        let _ = app.cmd_tx.send(EngineCommand::ToggleMasterRecord);
                    }
                    KeyCode::Char('/') | KeyCode::Char('f') => {
                        app.file_input_mode = true;
                        app.file_input_buf.clear();
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => app.move_selection(1),
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => app.move_selection(-1),
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                        app.adjust_selected(1.0)
                    }
                    KeyCode::Left | KeyCode::Char('h') => app.adjust_selected(-1.0),
                    KeyCode::Char('L') | KeyCode::PageUp => app.adjust_selected(4.0),
                    KeyCode::Char('H') | KeyCode::PageDown => app.adjust_selected(-4.0),
                    KeyCode::Char('[') => {
                        let _ = app
                            .cmd_tx
                            .send(EngineCommand::SwapPedalSlots { slot_a: 0, slot_b: 1 });
                    }
                    KeyCode::Char(']') => {
                        let _ = app
                            .cmd_tx
                            .send(EngineCommand::SwapPedalSlots { slot_a: 1, slot_b: 2 });
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn draw_ui(f: &mut Frame, app: &TuiApp) {
    let snap = app.telemetry.read().clone();
    let size = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Top header & Tabs
            Constraint::Min(14),   // Main Tab Content
            Constraint::Length(3), // Footer / Master Recorder & Controls Help
        ])
        .split(size);

    // 1. Header Bar + Tabs
    let header_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(chunks[0]);

    let titles = vec![
        Line::from(" [1] LAB INSTRUMENTS & SOURCES "),
        Line::from(" [2] PEDALBOARD / EFFECT CHAIN "),
        Line::from(" [3] TASCAM 4-TRACK RECORDER & SESSIONS "),
    ];
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " KLANGBENCH AUDIO WORKSTATION | Web UI: http://127.0.0.1:{} ",
                    app.web_port
                ))
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .select(app.tab as usize)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Amber.into_ratatui())
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, header_cols[0]);

    let rec_badge = if snap.is_recording {
        Span::styled(
            format!(" ● REC {:.1}s [{}] ", snap.recording_seconds, snap.recording_filename),
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!(" ■ STANDBY | {} ({} Hz) ", snap.audio_device_name, snap.sample_rate),
            Style::default().fg(Color::Green),
        )
    };
    let status_box = Paragraph::new(Line::from(vec![
        rec_badge,
        Span::raw(format!(
            " | L:{:>4.2} R:{:>4.2}",
            snap.master_peak_l, snap.master_peak_r
        )),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" MASTER TELEMETRY & 24-BIT WAV EXPORT "),
    );
    f.render_widget(status_box, header_cols[1]);

    // 2. Main Tab Body
    match app.tab {
        ActiveTab::LabInstruments => draw_tab_lab(f, chunks[1], app, &snap),
        ActiveTab::Pedalboard => draw_tab_pedalboard(f, chunks[1], app, &snap),
        ActiveTab::TascamRecorder => draw_tab_tascam(f, chunks[1], app, &snap),
    }

    // 3. Footer Bar
    let footer_text = if app.file_input_mode {
        format!(
            " LOAD SAMPLE PATH (.wav/.mp3/.flac): {}_  [Enter=Load, Esc=Cancel]",
            app.file_input_buf
        )
    } else {
        " KEYS: [1/2/3] Switch Tab | [j/k or ↑/↓/Tab] Select Param | [h/l or ←/→] Adjust | [SPACE/R] Master WAV Rec | [f] Load Audio File | [q] Quit".to_string()
    };
    let footer_style = if app.file_input_mode {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let footer = Paragraph::new(footer_text).style(footer_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(footer, chunks[2]);
}

trait CustomColorExt {
    fn into_ratatui(self) -> Color;
}
enum CustomCol {
    Amber,
}
impl ColorExt for Color {}
trait ColorExt {
    #[allow(non_upper_case_globals)]
    const Amber: CustomCol = CustomCol::Amber;
}
impl CustomColorExt for CustomCol {
    fn into_ratatui(self) -> Color {
        Color::Rgb(255, 184, 48)
    }
}

fn draw_tab_lab(f: &mut Frame, area: Rect, app: &TuiApp, snap: &TelemetryFrame) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    let sel = app.selected_idx[0];
    let mut items = Vec::new();
    let mut push_param = |idx: usize, group: &str, name: &str, val: String| {
        let active = idx == sel;
        let prefix = if active { "▶ " } else { "  " };
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 184, 48))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{}{:<12} ", prefix, group), style),
            Span::styled(format!("{:<22}: ", name), style),
            Span::styled(val, style),
        ])));
    };

    // 1. TAG-101
    push_param(
        0,
        "[TAG-101]",
        "Oscillator Power",
        if snap.tag101.enabled { "ON" } else { "OFF" }.into(),
    );
    push_param(
        1,
        "[TAG-101]",
        "Frequency Dial",
        format!("{:.1} ({:.1} Hz)", snap.tag101.dial_freq, snap.tag101_actual_hz),
    );
    push_param(2, "[TAG-101]", "Decade Multiplier", snap.tag101.decade.label().into());
    push_param(3, "[TAG-101]", "Output Waveform", format!("{:?}", snap.tag101.waveform));
    push_param(4, "[TAG-101]", "Step Attenuator", format!("{} dB", snap.tag101.attenuator_db));
    push_param(
        5,
        "[TAG-101]",
        "Fine Output Level",
        format!("{:.2} V (Sat >1.0V)", snap.tag101.fine_level),
    );

    // 2. B&K 2107
    push_param(
        6,
        "[B&K 2107]",
        "Analyzer Power",
        if snap.bk2107.enabled { "ON" } else { "OFF" }.into(),
    );
    push_param(
        7,
        "[B&K 2107]",
        "Center Frequency",
        format!("{:.1} Hz", snap.bk2107.center_freq_hz),
    );
    push_param(
        8,
        "[B&K 2107]",
        "Resonant Q Factor",
        format!("Q = {:.1}", snap.bk2107.q_factor),
    );
    push_param(
        9,
        "[B&K 2107]",
        "Input Sensitivity",
        format!("{:+.1} dB", snap.bk2107.sensitivity_db),
    );
    push_param(
        10,
        "[B&K 2107]",
        "Ballistic Meter Mode",
        if snap.bk2107.fast_peak_meter {
            "FAST PEAK"
        } else {
            "SLOW RMS"
        }
        .into(),
    );

    // 3. PAR 124A Lock-In
    push_param(
        11,
        "[PAR 124A]",
        "Lock-In Power",
        if snap.par124a.enabled { "ON" } else { "OFF" }.into(),
    );
    push_param(12, "[PAR 124A]", "Input Coupling", format!("{:?}", snap.par124a.coupling));
    push_param(13, "[PAR 124A]", "Signal Channel Mode", snap.par124a.filter_mode.label().into());
    push_param(
        14,
        "[PAR 124A]",
        "Signal Filter Freq",
        format!("{:.1} Hz", snap.par124a.filter_freq_hz),
    );
    push_param(15, "[PAR 124A]", "Signal Filter Q", format!("Q = {:.1}", snap.par124a.filter_q));
    push_param(
        16,
        "[PAR 124A]",
        "Reference Source",
        if snap.par124a.external_ref {
            "EXT (TAG-101)"
        } else {
            "INT OSC"
        }
        .into(),
    );
    push_param(
        17,
        "[PAR 124A]",
        "Phase Quadrant",
        format!("{}°", snap.par124a.phase_quadrant as u16 * 90),
    );
    push_param(
        18,
        "[PAR 124A]",
        "10-Turn Phase Vernier",
        format!("{:.1}° (Tot {:.1}°)", snap.par124a.phase_vernier_deg, snap.par124a.total_phase_deg()),
    );
    push_param(
        19,
        "[PAR 124A]",
        "Output Time Constant",
        format!("{:.3} s", snap.par124a.time_constant_sec()),
    );
    push_param(
        20,
        "[PAR 124A]",
        "Dynamic Tradeoff/Slope",
        format!(
            "{} | {}",
            snap.par124a.dynamic_reserve.label(),
            if snap.par124a.slope_12db { "12dB/oct" } else { "6dB/oct" }
        ),
    );

    // 4. B&K 1621 Filter
    push_param(21, "[B&K 1621]", "Bandwidth Mode", snap.bk1621.bandwidth.label().into());
    push_param(22, "[B&K 1621]", "Frequency Decade", snap.bk1621.decade.label().into());
    push_param(
        23,
        "[B&K 1621]",
        "Tuning Dial",
        format!("{:.2} ({:.1} Hz, φ={:+.0}°)", snap.bk1621.dial_tuning, snap.bk1621_current_hz, snap.bk1621_phase_deg),
    );
    push_param(
        24,
        "[B&K 1621]",
        "Input Voltage Range",
        if snap.bk1621.range_0_2v { "0.2 V RMS" } else { "1.0 V RMS" }.into(),
    );
    push_param(
        25,
        "[B&K 1621]",
        "Auto Decade Sweep",
        if snap.bk1621.auto_sweep { "SWEEPING" } else { "MANUAL" }.into(),
    );

    // 5. Koma Strom Mobile
    push_param(26, "[STROM MOB]", "LFO 1 Rate", format!("{:.2} Hz", snap.strom.lfo1_rate_hz));
    push_param(
        27,
        "[STROM MOB]",
        "LFO 1 Shape -> Target",
        format!("{} -> {}", snap.strom.lfo1_shape.label(), snap.strom.lfo1_target.label()),
    );
    push_param(
        28,
        "[STROM MOB]",
        "Power Supply Sag Rail",
        format!("{:.1} V", snap.strom.supply_voltage),
    );
    push_param(
        29,
        "[STROM MOB]",
        "50/60Hz Mains Ripple",
        format!("{:.0}%", snap.strom.ripple_hum * 100.0),
    );

    // 6. Function Gen
    push_param(30, "[FUNC GEN]", "Waveform", snap.func_gen.waveform.label().into());
    push_param(31, "[FUNC GEN]", "Frequency", format!("{:.1} Hz", snap.func_gen.freq_hz));
    push_param(32, "[FUNC GEN]", "PWM Duty / FM", format!("PWM {:.0}%", snap.func_gen.pwm_duty * 100.0));

    // 8. Dictaphone
    push_param(
        33,
        "[DICTAPHON]",
        "Micro-Cassette Play",
        format!("{} ({})", if snap.dictaphone.playing { "PLAY" } else { "STOP" }, snap.dictaphone.loaded_name),
    );
    push_param(
        34,
        "[DICTAPHON]",
        "Varispeed (±2 Oct)",
        format!("{:+.2} Oct", snap.dictaphone.speed_octaves),
    );
    push_param(
        35,
        "[DICTAPHON]",
        "Direction / Loop Preset",
        format!(
            "{} | [→ Next Loop, ← Rev]",
            if snap.dictaphone.reverse { "REVERSE" } else { "FORWARD" }
        ),
    );

    // 7. CRT Oscilloscope
    push_param(
        36,
        "[CRT SCOPE]",
        "Timebase (ms/div)",
        format!("{:.2} ms/div", snap.scope.timebase_ms_div),
    );
    push_param(
        37,
        "[CRT SCOPE]",
        "Volts/div & Trigger",
        format!("{:.2} V/div | {}", snap.scope.volts_per_div, snap.scope.trigger_mode.label()),
    );

    // Scroll list window around selected item
    let visible_height = cols[0].height.saturating_sub(2) as usize;
    let start_idx = if sel >= visible_height / 2 {
        (sel - visible_height / 2).min(items.len().saturating_sub(visible_height))
    } else {
        0
    };
    let sliced: Vec<ListItem> = items
        .into_iter()
        .skip(start_idx)
        .take(visible_height.max(10))
        .collect();

    let list = List::new(sliced).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" LABORATORY INSTRUMENTS & PARAMETERS (Use ↑/↓ & ←/→) "),
    );
    f.render_widget(list, cols[0]);

    // Right Column: Live Braille CRT Oscilloscope + Ballistic Meters
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(cols[1]);

    // Braille CRT Oscilloscope
    let scope_color = match snap.scope.phosphor {
        crate::dsp::instruments::PhosphorColor::GreenP31 => Color::LightGreen,
        crate::dsp::instruments::PhosphorColor::AmberP7 => Color::Rgb(255, 184, 48),
    };
    let ch1 = snap.scope_ch1.clone();
    let ch2 = snap.scope_ch2.clone();
    let thresh = snap.scope.trigger_threshold as f64;

    let canvas = Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " CRT OSCILLOSCOPE [{:.1}ms/div | {:.2}V/div | {}] ",
                    snap.scope.timebase_ms_div,
                    snap.scope.volts_per_div,
                    if snap.scope_triggered { "TRIG'D" } else { "FREE" }
                ))
                .border_style(Style::default().fg(scope_color)),
        )
        .marker(Marker::Braille)
        .x_bounds([0.0, 255.0])
        .y_bounds([-1.25, 1.25])
        .paint(move |ctx| {
            // Graticule center lines
            ctx.draw(&CanvasLine {
                x1: 0.0,
                y1: 0.0,
                x2: 255.0,
                y2: 0.0,
                color: Color::DarkGray,
            });
            ctx.draw(&CanvasLine {
                x1: 128.0,
                y1: -1.25,
                x2: 128.0,
                y2: 1.25,
                color: Color::DarkGray,
            });
            // Trigger threshold line
            ctx.draw(&CanvasLine {
                x1: 0.0,
                y1: thresh,
                x2: 255.0,
                y2: thresh,
                color: Color::Red,
            });

            // Channel 2 (Reference / LFO trace)
            for i in 0..ch2.len().saturating_sub(1) {
                ctx.draw(&CanvasLine {
                    x1: i as f64,
                    y1: ch2[i] as f64,
                    x2: (i + 1) as f64,
                    y2: ch2[i + 1] as f64,
                    color: Color::Cyan,
                });
            }

            // Channel 1 (Main Signal trace)
            for i in 0..ch1.len().saturating_sub(1) {
                ctx.draw(&CanvasLine {
                    x1: i as f64,
                    y1: ch1[i] as f64,
                    x2: (i + 1) as f64,
                    y2: ch1[i + 1] as f64,
                    color: scope_color,
                });
            }
        });
    f.render_widget(canvas, right_rows[0]);

    // Ballistic Meters & Overload Lamps Panel
    let meter_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .split(right_rows[1]);

    let bk_ratio = (snap.bk2107_meter / 1.2).clamp(0.0, 1.0) as f64;
    let bk_gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            " B&K 2107 BALLISTIC METER ({}) {} ",
            if snap.bk2107.fast_peak_meter { "FAST PEAK" } else { "SLOW RMS" },
            if snap.bk2107_overload { "[OVERLOAD!]" } else { "" }
        )))
        .gauge_style(Style::default().fg(if snap.bk2107_overload {
            Color::Red
        } else {
            Color::Yellow
        }))
        .ratio(bk_ratio);
    f.render_widget(bk_gauge, meter_rows[0]);

    let par_ratio = ((snap.par124a_demod_meter + 1.5) / 3.0).clamp(0.0, 1.0) as f64;
    let par_gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            " PAR 124A PSD DEMOD [{:+.2}V] PreOVL:{} PostOVL:{} ",
            snap.par124a_demod_meter,
            if snap.par124a_pre_overload { "●" } else { "○" },
            if snap.par124a_post_overload { "●" } else { "○" }
        )))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(par_ratio);
    f.render_widget(par_gauge, meter_rows[1]);

    let bk1621_info = Paragraph::new(format!(
        " B&K 1621: {:.1} Hz | Phase: {:+.1}° | OVL: {} | STROM LFO1:{:+.2} LFO2:{:+.2} Sag:{:.0}%",
        snap.bk1621_current_hz,
        snap.bk1621_phase_deg,
        if snap.bk1621_overload { "● OVERLOAD" } else { "○ OK" },
        snap.strom_lfo1_val,
        snap.strom_lfo2_val,
        snap.strom_sag_factor * 100.0
    ))
    .block(Block::default().borders(Borders::ALL).title(" LAB TELEMETRY "));
    f.render_widget(bk1621_info, meter_rows[2]);
}

fn draw_tab_pedalboard(f: &mut Frame, area: Rect, app: &TuiApp, snap: &TelemetryFrame) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(10)])
        .split(area);

    // Top Signal Chain Routing Diagram
    let order = snap.pedalboard.order;
    let chain_str = format!(
        "  LAB SOURCES ──▶ [Slot 1: {}] ──▶ [Slot 2: {}] ──▶ [Slot 3: {}] ──▶ [Slot 4: {}] ──▶ TASCAM / MASTER",
        order[0].label(),
        order[1].label(),
        order[2].label(),
        order[3].label()
    );
    let chain_box = Paragraph::new(vec![
        Line::from(Span::styled(
            chain_str,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::from(
            "  Tip: Select Slot Swap rows below (or press '[' / ']') to dynamically reorder the FX chain!",
        ),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" MODULAR REORDERABLE PEDALBOARD CHAIN "),
    );
    f.render_widget(chain_box, rows[0]);

    let sel = app.selected_idx[1];
    let mut items = Vec::new();
    let mut push_row = |idx: usize, pedal: &str, param: &str, val: String| {
        let active = idx == sel;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 184, 48))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(if active { "▶ " } else { "  " }, style),
            Span::styled(format!("{:<22} ", pedal), style),
            Span::styled(format!("{:<24}: ", param), style),
            Span::styled(val, style),
        ])));
    };

    push_row(
        0,
        "[CHAIN ORDER]",
        "Swap Slot 1 <-> Slot 2",
        format!("{} <-> {}", order[0].short_label(), order[1].short_label()),
    );
    push_row(
        1,
        "[CHAIN ORDER]",
        "Swap Slot 2 <-> Slot 3",
        format!("{} <-> {}", order[1].short_label(), order[2].short_label()),
    );
    push_row(
        2,
        "[CHAIN ORDER]",
        "Swap Slot 3 <-> Slot 4",
        format!("{} <-> {}", order[2].short_label(), order[3].short_label()),
    );

    // 1. Stereo Chorus
    let ch = &snap.pedalboard.chorus;
    push_row(
        3,
        "[ULTRA CHORUS]",
        "Stomp Bypass",
        if ch.bypass { "BYPASSED" } else { "ACTIVE ●" }.into(),
    );
    push_row(4, "[ULTRA CHORUS]", "LFO Rate (Anti-Phase)", format!("{:.2} Hz", ch.rate_hz));
    push_row(5, "[ULTRA CHORUS]", "Mod Depth (5-30 ms)", format!("{:.0}%", ch.depth * 100.0));
    push_row(6, "[ULTRA CHORUS]", "Tone / EQ", format!("{:.0}%", ch.tone * 100.0));
    push_row(7, "[ULTRA CHORUS]", "Wet / Dry Balance", format!("{:.0}%", ch.wet_dry * 100.0));

    // 2. Vintage Digital Delay
    let dl = &snap.pedalboard.delay;
    push_row(
        8,
        "[VINTAGE DELAY]",
        "Stomp Bypass",
        if dl.bypass { "BYPASSED" } else { "ACTIVE ●" }.into(),
    );
    push_row(9, "[VINTAGE DELAY]", "Delay Time (10-1500ms)", format!("{:.0} ms", dl.delay_ms));
    push_row(
        10,
        "[VINTAGE DELAY]",
        "Feedback (Self-Osc >100%)",
        format!("{:.0}%", dl.feedback * 100.0),
    );
    push_row(
        11,
        "[VINTAGE DELAY]",
        "Stereo Mode",
        if dl.ping_pong { "PING-PONG" } else { "STEREO" }.into(),
    );
    push_row(12, "[VINTAGE DELAY]", "Wet / Dry Mix", format!("{:.0}%", dl.mix * 100.0));

    // 3. Cooper FX Outward MK2
    let ow = &snap.pedalboard.outward;
    push_row(
        13,
        "[OUTWARD MK2]",
        "Stomp Bypass",
        if ow.bypass { "BYPASSED" } else { "ACTIVE ●" }.into(),
    );
    push_row(14, "[OUTWARD MK2]", "Granular Mode", ow.mode.label().into());
    push_row(15, "[OUTWARD MK2]", "Grain Size (5-500ms)", format!("{:.0} ms", ow.grain_size_ms));
    push_row(
        16,
        "[OUTWARD MK2]",
        "Grain Pitch Shifter",
        format!("{:+.1} st", ow.pitch_semitones),
    );
    push_row(17, "[OUTWARD MK2]", "Wet / Dry Mix", format!("{:.0}%", ow.mix * 100.0));

    // 4. Alesis Wedge Reverb
    let rv = &snap.pedalboard.reverb;
    push_row(
        18,
        "[ALESIS WEDGE]",
        "Unit Bypass",
        if rv.bypass { "BYPASSED" } else { "ACTIVE ●" }.into(),
    );
    push_row(19, "[ALESIS WEDGE]", "8-FDN Room Size", format!("{:.0}%", rv.room_size * 100.0));
    push_row(20, "[ALESIS WEDGE]", "Decay Time", format!("{:.0}%", rv.decay_time * 100.0));
    push_row(21, "[ALESIS WEDGE]", "Pre-Delay Time", format!("{:.0} ms", rv.pre_delay_ms));
    push_row(22, "[ALESIS WEDGE]", "Reverb Mix", format!("{:.0}%", rv.mix * 100.0));

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" PEDALBOARD PARAMETERS "),
    );
    f.render_widget(list, rows[1]);
}

fn draw_tab_tascam(f: &mut Frame, area: Rect, app: &TuiApp, snap: &TelemetryFrame) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);

    let sel = app.selected_idx[2];
    let mut items = Vec::new();
    let mut push_item = |idx: usize, label: &str, val: String| {
        let active = idx == sel;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 184, 48))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(if active { "▶ " } else { "  " }, style),
            Span::styled(format!("{:<28}: ", label), style),
            Span::styled(val, style),
        ])));
    };

    for ch in 0..4 {
        let base = ch * 6;
        let cp = &snap.tascam.channels[ch];
        push_item(
            base,
            &format!("Track {} Record Arm", ch + 1),
            if cp.rec_arm { "● REC ARMED" } else { "SAFE" }.into(),
        );
        push_item(
            base + 1,
            &format!("Track {} Input Source", ch + 1),
            cp.input_source.label().into(),
        );
        push_item(
            base + 2,
            &format!("Track {} Level Trim", ch + 1),
            format!("{:.2}", cp.level_trim),
        );
        push_item(
            base + 3,
            &format!("Track {} Stereo Pan", ch + 1),
            format!("{:+.2}", cp.pan),
        );
        push_item(
            base + 4,
            &format!("Track {} Mute", ch + 1),
            if cp.mute { "MUTED" } else { "ON" }.into(),
        );
        push_item(
            base + 5,
            &format!("Track {} Solo", ch + 1),
            if cp.solo { "SOLO ●" } else { "OFF" }.into(),
        );
    }

    push_item(
        24,
        "[TRANSPORT] Play / Stop",
        snap.tascam.transport.label().into(),
    );
    push_item(
        25,
        "[TRANSPORT] Rewind / FFWD",
        "[← REW | → FFWD]".into(),
    );
    push_item(
        26,
        "[TAPE DECK] Varispeed (±15%)",
        format!("{:+.1}%", snap.tascam.varispeed_pct),
    );
    push_item(
        27,
        "[TAPE DECK] Oxide Saturation k",
        format!("tanh({:.1} * x)", snap.tascam.tape_drive),
    );
    push_item(
        28,
        "[TAPE DECK] Crosstalk Matrix",
        format!("{:.1}% adjacent bleed", snap.tascam.crosstalk_amt * 100.0),
    );
    push_item(
        29,
        "[MASTER WAV] Session Recorder",
        if snap.is_recording {
            format!("● RECORDING ({:.1}s)", snap.recording_seconds)
        } else {
            "Press ENTER or SPACE to Record 24-bit WAV".into()
        },
    );

    let visible_height = cols[0].height.saturating_sub(2) as usize;
    let start_idx = if sel >= visible_height / 2 {
        (sel - visible_height / 2).min(items.len().saturating_sub(visible_height))
    } else {
        0
    };
    let sliced: Vec<ListItem> = items
        .into_iter()
        .skip(start_idx)
        .take(visible_height.max(10))
        .collect();

    let list = List::new(sliced).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" TASCAM PORTASTUDIO 4-TRACK CASSETTE MIXER & TRANSPORT "),
    );
    f.render_widget(list, cols[0]);

    // Right Column: 4-Track VU Meters + Tape Counter + Master Session Export List
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Min(4),
        ])
        .split(cols[1]);

    for ch in 0..4 {
        let level = (snap.tascam_meters[ch] * 1.4).clamp(0.0, 1.0) as f64;
        let cp = &snap.tascam.channels[ch];
        let g = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(format!(
                " CH {} VU [{}] {} {} ",
                ch + 1,
                cp.input_source.label(),
                if cp.rec_arm { "●REC" } else { "" },
                if cp.mute { "MUTE" } else { "" }
            )))
            .gauge_style(Style::default().fg(if cp.rec_arm {
                Color::Red
            } else {
                Color::Rgb(255, 184, 48)
            }))
            .ratio(level);
        f.render_widget(g, right_rows[ch]);
    }

    let tape_gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(format!(
            " CASSETTE TAPE COUNTER: {:05.1}s | {} | Varispeed {:+.1}% ",
            snap.tascam_counter_sec,
            snap.tascam.transport.label(),
            snap.tascam.varispeed_pct
        )))
        .gauge_style(Style::default().fg(Color::Green))
        .ratio(snap.tascam_progress as f64);
    f.render_widget(tape_gauge, right_rows[4]);

    let mut rec_lines = vec![
        Line::from(format!("Current Status: {}", snap.recording_filename)),
        Line::from("Format: Stereo 24-bit / 48 kHz Uncompressed WAV (./recordings/)"),
    ];
    for saved in snap.saved_recordings.iter().rev().take(4) {
        rec_lines.push(Line::from(Span::styled(
            format!("✓ Saved: {}", saved),
            Style::default().fg(Color::LightGreen),
        )));
    }
    let rec_panel = Paragraph::new(rec_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" MASTER SESSION WAV EXPORTS "),
    );
    f.render_widget(rec_panel, right_rows[5]);
}
