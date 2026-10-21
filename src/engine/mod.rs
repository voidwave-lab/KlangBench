use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::RwLock;
use rtrb::{Consumer, Producer, RingBuffer};
use serde::{Deserialize, Serialize};

use crate::dsp::instruments::*;
use crate::dsp::pedals::*;
use crate::dsp::tascam::*;
use crate::dsp::{clamp, load_audio_file_mono, soft_clip, BallisticMeter};

/// Commands sent from TUI or Web UI to the real-time DSP engine
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "data")]
pub enum EngineCommand {
    // Master & Recording
    SetMasterVolume(f32),
    ToggleMasterRecord,
    SetMasterRecord(bool),

    // 1. TAG-101
    SetTag101(Tag101Params),
    // 2. B&K 2107
    SetBk2107(Bk2107Params),
    // 3. PAR 124A Lock-In
    SetPar124a(Par124aParams),
    // 4. B&K 1621 Filter
    SetBk1621(Bk1621Params),
    // 5. Koma Strom Mobile
    SetStrom(StromMobileParams),
    // 6. Lab Function Generator
    SetFuncGen(LabFunctionGenParams),
    // 7. CRT Oscilloscope
    SetScope(CrtOscilloscopeParams),
    // 8. Lo-Fi Tape Dictaphone
    SetDictaphone(TapeDictaphoneParams),
    SelectDictaphonePreset(usize),
    LoadDictaphoneFile(String),
    LoadDictaphoneSamples { name: String, samples: Vec<f32> },

    // Tab 2: Pedalboard
    SetChorus(ChorusParams),
    SetDelay(DelayParams),
    SetOutward(OutwardParams),
    SetReverb(WedgeReverbParams),
    SwapPedalSlots { slot_a: usize, slot_b: usize },

    // Tab 3: Tascam 4-Track
    SetTascam(TascamState),
    ClearTascamTape,
}

/// Complete synchronized telemetry + parameter snapshot broadcast to TUI and WebSockets at ~45 FPS
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryFrame {
    pub sample_rate: u32,
    pub audio_device_name: String,
    pub master_volume: f32,

    // Master Recorder status
    pub is_recording: bool,
    pub recording_filename: String,
    pub recording_seconds: f32,
    pub saved_recordings: Vec<String>,

    // Master Stereo Meters
    pub master_peak_l: f32,
    pub master_peak_r: f32,
    pub master_rms_l: f32,
    pub master_rms_r: f32,

    // Oscilloscope 256-sample triggered traces (Ch1 = Master L / Lab Mix, Ch2 = Reference / Mod)
    pub scope_ch1: Vec<f32>,
    pub scope_ch2: Vec<f32>,
    pub scope_triggered: bool,

    // Instrument Live Telemetry
    pub tag101_actual_hz: f32,
    pub bk2107_meter: f32,
    pub bk2107_overload: bool,
    pub par124a_demod_meter: f32,
    pub par124a_pre_overload: bool,
    pub par124a_post_overload: bool,
    pub bk1621_current_hz: f32,
    pub bk1621_phase_deg: f32,
    pub bk1621_overload: bool,
    pub strom_lfo1_val: f32,
    pub strom_lfo2_val: f32,
    pub strom_sag_factor: f32,
    pub dictaphone_playhead: f32,

    // Tascam 4-Track Live Telemetry
    pub tascam_meters: [f32; 4],
    pub tascam_counter_sec: f32,
    pub tascam_progress: f32,

    // Full Parameter States
    pub tag101: Tag101Params,
    pub bk2107: Bk2107Params,
    pub par124a: Par124aParams,
    pub bk1621: Bk1621Params,
    pub strom: StromMobileParams,
    pub func_gen: LabFunctionGenParams,
    pub scope: CrtOscilloscopeParams,
    pub dictaphone: TapeDictaphoneParams,
    pub pedalboard: PedalboardState,
    pub tascam: TascamState,
}

impl Default for TelemetryFrame {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            audio_device_name: "Initializing...".to_string(),
            master_volume: 0.75,
            is_recording: false,
            recording_filename: "Standby (Press SPACE or R to Record)".to_string(),
            recording_seconds: 0.0,
            saved_recordings: Vec::new(),
            master_peak_l: 0.0,
            master_peak_r: 0.0,
            master_rms_l: 0.0,
            master_rms_r: 0.0,
            scope_ch1: vec![0.0; 256],
            scope_ch2: vec![0.0; 256],
            scope_triggered: true,
            tag101_actual_hz: 220.0,
            bk2107_meter: 0.0,
            bk2107_overload: false,
            par124a_demod_meter: 0.0,
            par124a_pre_overload: false,
            par124a_post_overload: false,
            bk1621_current_hz: 650.0,
            bk1621_phase_deg: 0.0,
            bk1621_overload: false,
            strom_lfo1_val: 0.0,
            strom_lfo2_val: 0.0,
            strom_sag_factor: 0.95,
            dictaphone_playhead: 0.0,
            tascam_meters: [0.0; 4],
            tascam_counter_sec: 0.0,
            tascam_progress: 0.0,
            tag101: Tag101Params::default(),
            bk2107: Bk2107Params::default(),
            par124a: Par124aParams::default(),
            bk1621: Bk1621Params::default(),
            strom: StromMobileParams::default(),
            func_gen: LabFunctionGenParams::default(),
            scope: CrtOscilloscopeParams::default(),
            dictaphone: TapeDictaphoneParams::default(),
            pedalboard: PedalboardState::default(),
            tascam: TascamState::default(),
        }
    }
}

/// Real-Time DSP Processor running inside the audio thread
pub struct DspProcessor {
    pub sample_rate: f32,
    pub device_name: String,
    pub master_volume: f32,

    pub tag101: Tag101Generator,
    pub bk2107: Bk2107Analyzer,
    pub par124a: Par124aLockIn,
    pub bk1621: Bk1621Filter,
    pub strom: StromMobile,
    pub func_gen: LabFunctionGen,
    pub scope_params: CrtOscilloscopeParams,
    pub dictaphone: TapeDictaphone,
    pub pedalboard: Pedalboard,
    pub tascam: Tascam4Track,

    master_meter_l: BallisticMeter,
    master_meter_r: BallisticMeter,

    // Oscilloscope capture ring buffer
    scope_ring_ch1: Vec<f32>,
    scope_ring_ch2: Vec<f32>,
    scope_write_idx: usize,
    scope_decim_accum: f32,

    // Lock-free channels & shared state
    cmd_rx: Receiver<EngineCommand>,
    wav_producer: Producer<f32>,
    is_recording: Arc<AtomicBool>,
    recording_meta: Arc<RwLock<(String, f32, Vec<String>)>>,
    telemetry_state: Arc<RwLock<TelemetryFrame>>,
    samples_since_telemetry: usize,
}

impl DspProcessor {
    pub fn new(
        sample_rate: u32,
        device_name: String,
        cmd_rx: Receiver<EngineCommand>,
        wav_producer: Producer<f32>,
        is_recording: Arc<AtomicBool>,
        recording_meta: Arc<RwLock<(String, f32, Vec<String>)>>,
        telemetry_state: Arc<RwLock<TelemetryFrame>>,
    ) -> Self {
        let mut dictaphone = TapeDictaphone::default();
        dictaphone.generate_builtin_tape_loop(0, sample_rate);
        let mut tascam = Tascam4Track::default();
        tascam.seed_initial_cassette_tracks(sample_rate as usize);

        Self {
            sample_rate: sample_rate as f32,
            device_name,
            master_volume: 0.75,
            tag101: Tag101Generator::default(),
            bk2107: Bk2107Analyzer::default(),
            par124a: Par124aLockIn::default(),
            bk1621: Bk1621Filter::default(),
            strom: StromMobile::default(),
            func_gen: LabFunctionGen::default(),
            scope_params: CrtOscilloscopeParams::default(),
            dictaphone,
            pedalboard: Pedalboard::default(),
            tascam,
            master_meter_l: BallisticMeter::default(),
            master_meter_r: BallisticMeter::default(),
            scope_ring_ch1: vec![0.0; 2048],
            scope_ring_ch2: vec![0.0; 2048],
            scope_write_idx: 0,
            scope_decim_accum: 0.0,
            cmd_rx,
            wav_producer,
            is_recording,
            recording_meta,
            telemetry_state,
            samples_since_telemetry: 0,
        }
    }

    pub fn poll_commands(&mut self) {
        let mut received_any = false;
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            received_any = true;
            match cmd {
                EngineCommand::SetMasterVolume(v) => {
                    self.master_volume = clamp(v, 0.0, 1.5);
                }
                EngineCommand::ToggleMasterRecord => {
                    let prev = self.is_recording.load(Ordering::SeqCst);
                    self.is_recording.store(!prev, Ordering::SeqCst);
                }
                EngineCommand::SetMasterRecord(rec) => {
                    self.is_recording.store(rec, Ordering::SeqCst);
                }
                EngineCommand::SetTag101(p) => self.tag101.params = p,
                EngineCommand::SetBk2107(p) => self.bk2107.params = p,
                EngineCommand::SetPar124a(p) => self.par124a.params = p,
                EngineCommand::SetBk1621(p) => self.bk1621.params = p,
                EngineCommand::SetStrom(p) => self.strom.params = p,
                EngineCommand::SetFuncGen(p) => self.func_gen.params = p,
                EngineCommand::SetScope(p) => self.scope_params = p,
                EngineCommand::SetDictaphone(p) => self.dictaphone.params = p,
                EngineCommand::SelectDictaphonePreset(idx) => {
                    self.dictaphone
                        .generate_builtin_tape_loop(idx, self.sample_rate as u32);
                }
                EngineCommand::LoadDictaphoneFile(path_str) => {
                    let path = Path::new(path_str.trim());
                    if let Ok(samples) = load_audio_file_mono(path, self.sample_rate as u32) {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Custom Sample")
                            .to_string();
                        self.dictaphone.set_custom_buffer(samples, name);
                    }
                }
                EngineCommand::LoadDictaphoneSamples { name, samples } => {
                    self.dictaphone.set_custom_buffer(samples, name);
                }
                EngineCommand::SetChorus(p) => self.pedalboard.chorus.params = p,
                EngineCommand::SetDelay(p) => self.pedalboard.delay.params = p,
                EngineCommand::SetOutward(p) => self.pedalboard.outward.params = p,
                EngineCommand::SetReverb(p) => self.pedalboard.reverb.params = p,
                EngineCommand::SwapPedalSlots { slot_a, slot_b } => {
                    self.pedalboard.swap_slots(slot_a, slot_b);
                }
                EngineCommand::SetTascam(s) => self.tascam.state = s,
                EngineCommand::ClearTascamTape => self.tascam.clear_all_tracks(),
            }
        }
        if received_any {
            self.samples_since_telemetry = 1_000_000;
        }
    }

    #[inline]
    pub fn process_stereo_frame(&mut self) -> (f32, f32) {
        let sr = self.sample_rate;

        // 1. Koma Strom Mobile (Power Supply Sag, Mains Hum & Dual LFOs)
        let strom_out = self.strom.process(sr);
        let sag = strom_out.supply_sag_factor;
        let sp = &self.strom.params;

        // 2. Primary Sound Sources (TAG-101, Function Generator, Lo-Fi Dictaphone)
        let (tag_sig, tag_phase) =
            self.tag101
                .process(strom_out.cv_for(ModTarget::Tag101Freq, sp), sag, sr);

        let func_sig = self.func_gen.process(
            tag_sig,
            strom_out.cv_for(ModTarget::FuncGenFreq, sp),
            sr,
        );

        let dict_sig =
            self.dictaphone
                .process(strom_out.cv_for(ModTarget::DictaphoneSpeed, sp), sag, sr);

        // Lab source bus feeding the analyzers & filters
        let raw_sources_sum =
            (tag_sig * 0.55 + func_sig * 0.45 + dict_sig * 0.55 + strom_out.hum_sample) * 0.75;

        // 3. Laboratory Filters & Analyzers (B&K 2107, B&K 1621, PAR 124A Lock-In)
        let bk2107_sig = self.bk2107.process(
            raw_sources_sum,
            strom_out.cv_for(ModTarget::Bk2107Freq, sp),
            sag,
            sr,
        );

        let bk1621_sig = self.bk1621.process(
            raw_sources_sum,
            self.tag101.params.nominal_frequency_hz(),
            strom_out.cv_for(ModTarget::Bk1621Freq, sp),
            sag,
            sr,
        );

        let par124a_sig = self.par124a.process(
            raw_sources_sum,
            tag_phase,
            strom_out.cv_for(ModTarget::Par124aRef, sp),
            sag,
            sr,
        );

        // Combine Tab 1 Lab Instruments into Stereo Bus entering Tab 2 Pedalboard
        let lab_l = tag_sig * 0.32
            + func_sig * 0.25
            + dict_sig * 0.35
            + bk2107_sig * 0.48
            + bk1621_sig * 0.25
            + strom_out.hum_sample;
        let lab_r = tag_sig * 0.28
            + func_sig * 0.30
            + dict_sig * 0.35
            + bk1621_sig * 0.48
            + par124a_sig * 0.45
            + strom_out.hum_sample;

        // 4. Tab 2: Modular Reorderable Pedalboard Effect Chain
        let (ped_l, ped_r) = self.pedalboard.process(lab_l, lab_r, sr);

        // 5. Tab 3: Tascam 4-Track Cassette Recorder
        let src_bundle = SourceSignals {
            tag101: tag_sig,
            func_gen: func_sig,
            dictaphone: dict_sig,
            bk2107: bk2107_sig,
            bk1621: bk1621_sig,
            par124a: par124a_sig,
            pedal_bus: (ped_l + ped_r) * 0.5,
        };
        let (tape_l, tape_r, tascam_meters) = self.tascam.process(&src_bundle, sr);

        // Master Stereo Mix
        let out_l = soft_clip((ped_l * 0.72 + tape_l * 0.55) * self.master_volume, 1.05);
        let out_r = soft_clip((ped_r * 0.72 + tape_r * 0.55) * self.master_volume, 1.05);

        self.master_meter_l.process(out_l, true, sr);
        self.master_meter_r.process(out_r, true, sr);

        // Push to lock-free WAV Recorder ring buffer if recording is active
        if self.is_recording.load(Ordering::Relaxed) {
            let _ = self.wav_producer.push(out_l);
            let _ = self.wav_producer.push(out_r);
        }

        // Capture into Oscilloscope Ring Buffer with timebase-dependent decimation
        // 10 divisions across 256 samples => 25.6 samples per division
        let samples_per_div = (self.scope_params.timebase_ms_div * 0.001 * sr).max(2.56);
        let decim_step = (samples_per_div / 25.6).max(1.0);
        self.scope_decim_accum += 1.0;
        if self.scope_decim_accum >= decim_step {
            self.scope_decim_accum -= decim_step;
            let ring_len = self.scope_ring_ch1.len();
            let v_scale = (0.5 / self.scope_params.volts_per_div.max(0.05)).clamp(0.1, 10.0);
            self.scope_ring_ch1[self.scope_write_idx] = clamp(out_l * v_scale, -1.2, 1.2);
            self.scope_ring_ch2[self.scope_write_idx] =
                clamp((tag_sig * 0.6 + strom_out.lfo1_val * 0.4) * v_scale, -1.2, 1.2);
            self.scope_write_idx = (self.scope_write_idx + 1) % ring_len;
        }

        // Update Telemetry Snapshot at ~50 Hz
        self.samples_since_telemetry += 1;
        if self.samples_since_telemetry >= (sr as usize / 50).max(256) {
            self.samples_since_telemetry = 0;
            self.publish_telemetry(&strom_out, tascam_meters);
        }

        (out_l, out_r)
    }

    fn publish_telemetry(&self, strom_out: &StromModOutputs, tascam_meters: [f32; 4]) {
        if let Some(mut guard) = self.telemetry_state.try_write() {
            let (scope1, scope2, triggered) = self.extract_triggered_scope();
            let (rec_name, rec_secs, saved_list) = self.recording_meta.read().clone();

            *guard = TelemetryFrame {
                sample_rate: self.sample_rate as u32,
                audio_device_name: self.device_name.clone(),
                master_volume: self.master_volume,
                is_recording: self.is_recording.load(Ordering::Relaxed),
                recording_filename: rec_name,
                recording_seconds: rec_secs,
                saved_recordings: saved_list,
                master_peak_l: self.master_meter_l.peak_env,
                master_peak_r: self.master_meter_r.peak_env,
                master_rms_l: self.master_meter_l.rms_sq.sqrt(),
                master_rms_r: self.master_meter_r.rms_sq.sqrt(),
                scope_ch1: scope1,
                scope_ch2: scope2,
                scope_triggered: triggered,
                tag101_actual_hz: self.tag101.params.nominal_frequency_hz(),
                bk2107_meter: self.bk2107.meter.needle_pos,
                bk2107_overload: self.bk2107.overload,
                par124a_demod_meter: self.par124a.demod_meter,
                par124a_pre_overload: self.par124a.pre_overload,
                par124a_post_overload: self.par124a.post_overload,
                bk1621_current_hz: self.bk1621.current_freq_hz,
                bk1621_phase_deg: self.bk1621.phase_shift_deg,
                bk1621_overload: self.bk1621.overload_lamp,
                strom_lfo1_val: strom_out.lfo1_val,
                strom_lfo2_val: strom_out.lfo2_val,
                strom_sag_factor: strom_out.supply_sag_factor,
                dictaphone_playhead: self.dictaphone.playhead_norm(),
                tascam_meters,
                tascam_counter_sec: self.tascam.tape_counter_seconds(self.sample_rate),
                tascam_progress: self.tascam.tape_progress_norm(),
                tag101: self.tag101.params.clone(),
                bk2107: self.bk2107.params.clone(),
                par124a: self.par124a.params.clone(),
                bk1621: self.bk1621.params.clone(),
                strom: self.strom.params.clone(),
                func_gen: self.func_gen.params.clone(),
                scope: self.scope_params.clone(),
                dictaphone: self.dictaphone.params.clone(),
                pedalboard: self.pedalboard.snapshot(),
                tascam: self.tascam.state.clone(),
            };
        }
    }

    fn extract_triggered_scope(&self) -> (Vec<f32>, Vec<f32>, bool) {
        let n = 256;
        let ring_len = self.scope_ring_ch1.len();
        let thresh = self.scope_params.trigger_threshold;
        let mode = self.scope_params.trigger_mode;

        // Search backwards from write_idx - n for trigger crossing
        let mut trig_start = (self.scope_write_idx + ring_len - n) % ring_len;
        let mut found = false;

        if mode != TriggerMode::Auto {
            for offset in n..(n + 512) {
                let idx0 = (self.scope_write_idx + ring_len - offset) % ring_len;
                let idx1 = (idx0 + 1) % ring_len;
                let s0 = self.scope_ring_ch1[idx0];
                let s1 = self.scope_ring_ch1[idx1];

                let hit = match mode {
                    TriggerMode::RisingEdge | TriggerMode::Normal => s0 < thresh && s1 >= thresh,
                    TriggerMode::FallingEdge => s0 > thresh && s1 <= thresh,
                    TriggerMode::Auto => true,
                };
                if hit {
                    trig_start = idx0;
                    found = true;
                    break;
                }
            }
        } else {
            found = true;
        }

        let mut ch1 = Vec::with_capacity(n);
        let mut ch2 = Vec::with_capacity(n);
        for i in 0..n {
            let idx = (trig_start + i) % ring_len;
            ch1.push(self.scope_ring_ch1[idx]);
            ch2.push(self.scope_ring_ch2[idx]);
        }
        (ch1, ch2, found)
    }
}

/// Handle to keep the audio engine and background WAV recorder alive
pub struct EngineHandle {
    pub cmd_tx: Sender<EngineCommand>,
    pub telemetry: Arc<RwLock<TelemetryFrame>>,
    _stream: Option<cpal::Stream>,
}

/// Spawns the background Master WAV Session Recorder thread (24-bit / 48 kHz stereo WAV to `./recordings/`)
fn spawn_wav_recorder_thread(
    mut wav_consumer: Consumer<f32>,
    is_recording: Arc<AtomicBool>,
    recording_meta: Arc<RwLock<(String, f32, Vec<String>)>>,
    sample_rate: u32,
) {
    thread::Builder::new()
        .name("klangbench-wav-recorder".to_string())
        .spawn(move || {
            let rec_dir = PathBuf::from("./recordings");
            let _ = fs::create_dir_all(&rec_dir);

            let mut active_writer: Option<hound::WavWriter<std::io::BufWriter<fs::File>>> = None;
            let mut recorded_frames: u64 = 0;

            loop {
                let want_record = is_recording.load(Ordering::SeqCst);

                if want_record && active_writer.is_none() {
                    let _ = fs::create_dir_all(&rec_dir);
                    let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
                    let filename = format!("klangbench_{}.wav", stamp);
                    let filepath = rec_dir.join(&filename);

                    let spec = hound::WavSpec {
                        channels: 2,
                        sample_rate,
                        bits_per_sample: 24,
                        sample_format: hound::SampleFormat::Int,
                    };

                    if let Ok(writer) = hound::WavWriter::create(&filepath, spec) {
                        active_writer = Some(writer);
                        recorded_frames = 0;
                        let mut meta = recording_meta.write();
                        meta.0 = format!("./recordings/{}", filename);
                        meta.1 = 0.0;
                    } else {
                        is_recording.store(false, Ordering::SeqCst);
                    }
                } else if !want_record && active_writer.is_some() {
                    if let Some(writer) = active_writer.take() {
                        let _ = writer.finalize();
                    }
                    let mut meta = recording_meta.write();
                    let finished_name = meta.0.clone();
                    if !finished_name.is_empty() && finished_name.starts_with("./recordings/") {
                        meta.2.push(finished_name);
                    }
                    meta.0 = "Standby (Saved to ./recordings/)".to_string();
                }

                // Drain available samples from lock-free ring buffer
                let mut wrote_any = false;
                while wav_consumer.slots() >= 2 {
                    if let (Ok(l), Ok(r)) = (wav_consumer.pop(), wav_consumer.pop()) {
                        if let Some(ref mut writer) = active_writer {
                            let max_24bit = 8_388_607.0_f32;
                            let sl = (clamp(l, -1.0, 1.0) * max_24bit) as i32;
                            let sr = (clamp(r, -1.0, 1.0) * max_24bit) as i32;
                            let _ = writer.write_sample(sl);
                            let _ = writer.write_sample(sr);
                            recorded_frames += 1;
                            wrote_any = true;
                        }
                    }
                }

                if wrote_any {
                    if let Some(mut meta) = recording_meta.try_write() {
                        meta.1 = (recorded_frames as f32) / (sample_rate.max(1) as f32);
                    }
                } else {
                    thread::sleep(Duration::from_millis(8));
                }
            }
        })
        .expect("Failed to spawn WAV recorder thread");
}

/// Initializes the real-time `cpal` audio engine and lock-free recorder thread
pub fn start_audio_engine() -> EngineHandle {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let is_recording = Arc::new(AtomicBool::new(false));
    let recording_meta = Arc::new(RwLock::new((
        "Standby (SPACE / R to Record 24-bit WAV)".to_string(),
        0.0,
        Vec::new(),
    )));
    let telemetry = Arc::new(RwLock::new(TelemetryFrame::default()));

    // Attempt to initialize default cpal output device
    let host = cpal::default_host();
    if let Some(device) = host.default_output_device() {
        let dev_name = device.name().unwrap_or_else(|_| "CPAL Default Output".to_string());
        if let Ok(supported_cfg) = device.default_output_config() {
            let sample_rate = supported_cfg.sample_rate().0;
            let channels = supported_cfg.channels() as usize;
            let sample_format = supported_cfg.sample_format();
            let config: cpal::StreamConfig = supported_cfg.into();

            if matches!(sample_format, cpal::SampleFormat::F32 | cpal::SampleFormat::I16) {
                let (wav_prod, wav_cons) = RingBuffer::<f32>::new(48_000 * 2 * 4);
                spawn_wav_recorder_thread(
                    wav_cons,
                    Arc::clone(&is_recording),
                    Arc::clone(&recording_meta),
                    sample_rate,
                );

                let cmd_rx_clone = cmd_rx.clone();
                let mut dsp = DspProcessor::new(
                    sample_rate,
                    dev_name,
                    cmd_rx_clone,
                    wav_prod,
                    Arc::clone(&is_recording),
                    Arc::clone(&recording_meta),
                    Arc::clone(&telemetry),
                );

                let err_fn = |err| {
                    eprintln!("CPAL stream error: {}", err);
                };

                let stream_res = match sample_format {
                    cpal::SampleFormat::F32 => device.build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            dsp.poll_commands();
                            for frame in data.chunks_mut(channels.max(1)) {
                                let (l, r) = dsp.process_stereo_frame();
                                if frame.len() == 1 {
                                    frame[0] = (l + r) * 0.5;
                                } else {
                                    frame[0] = l;
                                    frame[1] = r;
                                    for extra in &mut frame[2..] {
                                        *extra = 0.0;
                                    }
                                }
                            }
                        },
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::I16 => device.build_output_stream(
                        &config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            dsp.poll_commands();
                            for frame in data.chunks_mut(channels.max(1)) {
                                let (l, r) = dsp.process_stereo_frame();
                                let li = (clamp(l, -1.0, 1.0) * 32767.0) as i16;
                                let ri = (clamp(r, -1.0, 1.0) * 32767.0) as i16;
                                if frame.len() == 1 {
                                    frame[0] = li / 2 + ri / 2;
                                } else {
                                    frame[0] = li;
                                    frame[1] = ri;
                                }
                            }
                        },
                        err_fn,
                        None,
                    ),
                    _ => unreachable!(),
                };

                if let Ok(stream) = stream_res {
                    if stream.play().is_ok() {
                        return EngineHandle {
                            cmd_tx,
                            telemetry,
                            _stream: Some(stream),
                        };
                    }
                }
            }
        }
    }

    // Fallback: Headless real-time 48 kHz DSP thread if no audio hardware is available
    let fallback_sr = 48_000;
    let (wav_prod2, wav_cons2) = RingBuffer::<f32>::new(48_000 * 2 * 4);
    spawn_wav_recorder_thread(
        wav_cons2,
        Arc::clone(&is_recording),
        Arc::clone(&recording_meta),
        fallback_sr,
    );

    let telemetry_clone = Arc::clone(&telemetry);
    thread::Builder::new()
        .name("klangbench-dsp-fallback".to_string())
        .spawn(move || {
            let mut dsp = DspProcessor::new(
                fallback_sr,
                "Virtual 48kHz Lab Clock (No Audio Hardware)".to_string(),
                cmd_rx,
                wav_prod2,
                is_recording,
                recording_meta,
                telemetry_clone,
            );
            let block_frames = 512;
            let block_dur = Duration::from_micros((block_frames * 1_000_000 / fallback_sr) as u64);
            loop {
                let start = Instant::now();
                dsp.poll_commands();
                for _ in 0..block_frames {
                    let _ = dsp.process_stereo_frame();
                }
                let elapsed = start.elapsed();
                if elapsed < block_dur {
                    thread::sleep(block_dur - elapsed);
                }
            }
        })
        .expect("Failed to spawn fallback DSP thread");

    EngineHandle {
        cmd_tx,
        telemetry,
        _stream: None,
    }
}
