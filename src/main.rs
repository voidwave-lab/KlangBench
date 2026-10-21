pub mod dsp;
pub mod engine;
pub mod tui;
pub mod web;

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut web_port: u16 = 8080;
    let mut headless = false;
    let mut self_test = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                if let Some(p_str) = args.get(i + 1) {
                    if let Ok(p) = p_str.parse::<u16>() {
                        web_port = p;
                    }
                    i += 1;
                }
            }
            "--headless" | "--web-only" => {
                headless = true;
            }
            "--self-test" => {
                self_test = true;
            }
            _ => {}
        }
        i += 1;
    }

    // 1. Start the real-time CPAL audio engine + lock-free 24-bit WAV session recorder
    let engine_handle = engine::start_audio_engine();

    if self_test {
        // Run a short automated verification of the live engine & WAV recorder
        std::thread::sleep(Duration::from_millis(120));
        let _ = engine_handle
            .cmd_tx
            .send(engine::EngineCommand::SetMasterRecord(true));
        std::thread::sleep(Duration::from_millis(250));
        let _ = engine_handle
            .cmd_tx
            .send(engine::EngineCommand::SetMasterRecord(false));
        std::thread::sleep(Duration::from_millis(80));
        let snap = engine_handle.telemetry.read();
        println!(
            "KlangBench Self-Test OK | Device: {} | SR: {} Hz | Scope samples: {}",
            snap.audio_device_name,
            snap.sample_rate,
            snap.scope_ch1.len()
        );
        return Ok(());
    }

    // 2. Spawn the embedded Axum + WebSocket Web GUI server
    web::spawn_web_server(
        engine_handle.cmd_tx.clone(),
        Arc::clone(&engine_handle.telemetry),
        web_port,
    );

    // 3. Launch Ratatui TUI if running in an interactive terminal, otherwise run headless Web server
    if !headless && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        tui::run_tui(
            engine_handle.cmd_tx.clone(),
            Arc::clone(&engine_handle.telemetry),
            web_port,
        )?;
    } else {
        println!("==================================================================");
        println!(" KLANGBENCH — Audio Generation & Test Equipment Workstation");
        println!(" Web Interface running at: http://127.0.0.1:{}", web_port);
        println!(" Press Ctrl+C to stop.");
        println!("==================================================================");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::dsp::instruments::*;
    use super::dsp::pedals::*;
    use super::dsp::tascam::*;

    #[test]
    fn test_tag101_decades_and_saturation() {
        let mut tag = Tag101Generator::default();
        tag.params.dial_freq = 44.0;
        tag.params.decade = Tag101Decade::X10;
        assert!((tag.params.nominal_frequency_hz() - 440.0).abs() < 0.01);

        tag.params.decade = Tag101Decade::X10k;
        tag.params.dial_freq = 100.0;
        assert!((tag.params.nominal_frequency_hz() - 1_000_000.0).abs() < 1.0);

        tag.params.decade = Tag101Decade::X10;
        tag.params.fine_level = 1.3;
        let mut peak = 0.0_f32;
        for _ in 0..1024 {
            let (s, _) = tag.process(0.0, 1.0, 48_000.0);
            assert!(s.is_finite());
            peak = peak.max(s.abs());
        }
        assert!(peak > 0.5 && peak < 1.3);
    }

    #[test]
    fn test_bk2107_and_bk1621_filters() {
        let mut bk2107 = Bk2107Analyzer::default();
        bk2107.params.q_factor = 110.0; // Ultra-narrow Q > 100
        let mut bk1621 = Bk1621Filter::default();
        bk1621.params.bandwidth = Bk1621Bandwidth::Pct3;
        bk1621.params.auto_sweep = true;

        for i in 0..2048 {
            let x = ((i as f32) * 0.05).sin();
            let y1 = bk2107.process(x, 0.0, 1.0, 48_000.0);
            let y2 = bk1621.process(x, 440.0, 0.0, 1.0, 48_000.0);
            assert!(y1.is_finite());
            assert!(y2.is_finite());
        }
        assert!(bk2107.meter.needle_pos >= 0.0);
        assert!(bk1621.phase_shift_deg.abs() <= 90.0);
    }

    #[test]
    fn test_par124a_lock_in_modes() {
        let mut lockin = Par124aLockIn::default();
        for mode in [
            Par124aFilterMode::Flat,
            Par124aFilterMode::Bandpass,
            Par124aFilterMode::Notch,
            Par124aFilterMode::LowPass,
            Par124aFilterMode::HighPass,
        ] {
            lockin.params.filter_mode = mode;
            for i in 0..512 {
                let x = ((i as f32) * 0.029).sin();
                let out = lockin.process(x, (i as f32 * 0.029).fract(), 0.0, 1.0, 48_000.0);
                assert!(out.is_finite());
            }
        }
    }

    #[test]
    fn test_pedalboard_and_tascam() {
        let mut pb = Pedalboard::default();
        pb.swap_slots(0, 3);
        assert_eq!(pb.order[0], PedalId::Reverb);

        let mut tascam = Tascam4Track::default();
        tascam.state.channels[0].rec_arm = true;

        for i in 0..1024 {
            let s = ((i as f32) * 0.04).sin() * 0.5;
            let (pl, pr) = pb.process(s, -s, 48_000.0);
            assert!(pl.is_finite() && pr.is_finite());

            let src = SourceSignals {
                tag101: s,
                pedal_bus: (pl + pr) * 0.5,
                ..Default::default()
            };
            let (tl, tr, meters) = tascam.process(&src, 48_000.0);
            assert!(tl.is_finite() && tr.is_finite());
            assert!(meters[0] >= 0.0);
        }
    }
}
