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

use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, State};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;

use crate::engine::{EngineCommand, TelemetryFrame};

#[derive(Clone)]
pub struct WebAppState {
    pub cmd_tx: Sender<EngineCommand>,
    pub telemetry: Arc<RwLock<TelemetryFrame>>,
}

const INDEX_HTML: &str = include_str!("index.html");

pub fn spawn_web_server(
    cmd_tx: Sender<EngineCommand>,
    telemetry: Arc<RwLock<TelemetryFrame>>,
    port: u16,
) {
    let state = WebAppState { cmd_tx, telemetry };

    std::thread::Builder::new()
        .name("klangbench-axum-web".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime for Web GUI");

            rt.block_on(async move {
                let app = Router::new()
                    .route("/", get(index_handler))
                    .route("/ws", get(ws_handler))
                    .route("/api/upload-sample", post(upload_sample_handler))
                    .with_state(state);

                let addr = SocketAddr::from(([127, 0, 0, 1], port));
                if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                    let _ = axum::serve(listener, app).await;
                }
            });
        })
        .expect("Failed to spawn web server thread");
}

async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<WebAppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: WebAppState) {
    let (mut sender, mut receiver) = socket.split();

    let telemetry_ref = Arc::clone(&state.telemetry);
    let send_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(22)); // ~45 FPS
        loop {
            interval.tick().await;
            let json_str = {
                let snap = telemetry_ref.read();
                serde_json::to_string(&*snap).unwrap_or_default()
            };
            if sender.send(Message::Text(json_str)).await.is_err() {
                break;
            }
        }
    });

    let cmd_tx = state.cmd_tx.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(txt) = msg {
                if let Ok(cmd) = serde_json::from_str::<EngineCommand>(&txt) {
                    let _ = cmd_tx.send(cmd);
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
}

/// Decodes uploaded .wav / .mp3 / .flac bytes in memory via Symphonia and sends to Dictaphone
async fn upload_sample_handler(
    State(state): State<WebAppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field
            .file_name()
            .unwrap_or("uploaded_sample.wav")
            .to_string();
        if let Ok(bytes) = field.bytes().await {
            let sr = state.telemetry.read().sample_rate;
            if let Ok(samples) = decode_audio_bytes_mono(bytes.to_vec(), &filename, sr) {
                let _ = state.cmd_tx.send(EngineCommand::LoadDictaphoneSamples {
                    name: filename.clone(),
                    samples,
                });
                return Json(serde_json::json!({ "ok": true, "name": filename }));
            }
        }
    }
    Json(serde_json::json!({ "ok": false, "error": "Failed to decode audio file" }))
}

fn decode_audio_bytes_mono(data: Vec<u8>, filename: &str, target_sr: u32) -> anyhow::Result<Vec<f32>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let cursor = Cursor::new(data);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
    {
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
        .ok_or_else(|| anyhow::anyhow!("No track"))?;
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

    let mut raw_mono = Vec::new();
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        if let Ok(decoded) = decoder.decode(&packet) {
            let spec = *decoded.spec();
            let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
            buf.copy_interleaved_ref(decoded);
            for frame in buf.samples().chunks(channels) {
                let s: f32 = frame.iter().copied().sum();
                raw_mono.push(s / (frame.len() as f32));
            }
        }
        if raw_mono.len() > (src_sr as usize) * 60 {
            break;
        }
    }

    if raw_mono.is_empty() {
        anyhow::bail!("Empty audio");
    }

    if src_sr == target_sr {
        Ok(raw_mono)
    } else {
        let ratio = (src_sr as f64) / (target_sr as f64);
        let out_len = ((raw_mono.len() as f64) / ratio) as usize;
        let mut resampled = Vec::with_capacity(out_len);
        for i in 0..out_len {
            let pos = (i as f64) * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let s0 = raw_mono.get(idx).copied().unwrap_or(0.0);
            let s1 = raw_mono.get(idx + 1).copied().unwrap_or(s0);
            resampled.push(s0 + frac * (s1 - s0));
        }
        Ok(resampled)
    }
}
