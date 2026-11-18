//! OpenAI-compatible TTS server for sultson/voca (tiny-voca Rust port).
//!
//! POST /v1/audio/speech
//! GET  /health
//! GET  /v1/models

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tiny_voca::tts::Tts;
use tiny_voca::Options;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppConfig {
    api_key: Option<String>,
    require_api_key: bool,
    default_voice: String,
    host: String,
    port: u16,
}

impl AppConfig {
    fn from_env() -> Self {
        let api_key = std::env::var("API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "changeme");

        let require = match std::env::var("REQUIRE_API_KEY") {
            Ok(v) => !matches!(v.to_lowercase().as_str(), "0" | "false" | "no" | "off"),
            Err(_) => api_key.is_some(),
        };

        Self {
            api_key,
            require_api_key: require,
            default_voice: std::env::var("DEFAULT_VOICE").unwrap_or_else(|_| "F2".into()),
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3420),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    tts: Tts,
    config: AppConfig,
}

// ---------------------------------------------------------------------------
// OpenAI-compatible request / response
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SpeechRequest {
    /// Text to speak
    input: String,
    /// "f2" / "F2" / "m2" / "M2"  (also accepts "alloy", "echo" etc. as aliases)
    #[serde(default)]
    voice: Option<String>,
    /// Speaking rate 0.25 – 4.0
    #[serde(default = "default_speed")]
    speed: f32,
    /// "wav" (default) or "pcm" (raw s16le)
    #[serde(default = "default_format")]
    response_format: String,
    /// Optional pitch shift in semitones (extension, not in official OpenAI schema)
    #[serde(default)]
    pitch: Option<f32>,
    /// Optional pitch range multiplier 0–3
    #[serde(default)]
    pitch_range: Option<f32>,
    /// Noise seed
    #[serde(default)]
    seed: Option<u64>,
}

fn default_speed() -> f32 {
    1.0
}
fn default_format() -> String {
    "wav".into()
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    model: &'static str,
    voices: [&'static str; 2],
    sample_rate: u32,
}

#[derive(Serialize)]
struct ModelObject {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

#[derive(Serialize)]
struct ModelsResponse {
    object: &'static str,
    data: Vec<ModelObject>,
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    if !state.config.require_api_key {
        return Ok(next.run(request).await);
    }

    let expected = match &state.config.api_key {
        Some(k) => k.as_str(),
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": {"message": "API key required but not configured", "type": "auth_error"}})),
            ));
        }
    };

    let mut token: Option<&str> = None;

    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(rest) = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")) {
            token = Some(rest.trim());
        }
    }
    if token.is_none() {
        if let Some(key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
            token = Some(key.trim());
        }
    }

    match token {
        Some(t) if t == expected => Ok(next.run(request).await),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": {"message": "Invalid or missing API key", "type": "auth_error"}})),
        )),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        model: "tiny-voca",
        voices: ["F2", "M2"],
        sample_rate: state.tts.model.config.sample_rate as u32,
    })
}

async fn list_models() -> Json<ModelsResponse> {
    Json(ModelsResponse {
        object: "list",
        data: vec![ModelObject {
            id: "tiny-voca".into(),
            object: "model",
            created: 0,
            owned_by: "n4ze3m / sultson",
        }],
    })
}

fn normalize_voice(raw: &str, default: &str) -> String {
    let v = raw.trim().to_uppercase();
    match v.as_str() {
        "F2" | "FEMALE" | "ALLOY" | "NOVA" | "SHIMMER" => "F2".into(),
        "M2" | "MALE" | "ECHO" | "ONYX" | "FABLE" => "M2".into(),
        "" => default.to_uppercase(),
        other => other.to_string(), // let the model reject unknown
    }
}

async fn speech(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SpeechRequest>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let text = req.input.trim();
    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": {"message": "input is empty", "type": "invalid_request_error"}})),
        ));
    }

    let voice = normalize_voice(
        req.voice.as_deref().unwrap_or(""),
        &state.config.default_voice,
    );

    let speed = req.speed.clamp(0.25, 4.0);
    let pitch_shift = req
        .pitch
        .map(|semitones| semitones * (2f32.ln() / 12.0))
        .unwrap_or(0.0);
    let pitch_range = req.pitch_range.unwrap_or(1.0).clamp(0.0, 3.0);
    let seed = req.seed.unwrap_or(0);

    let options = Options {
        speed,
        pitch_shift,
        pitch_range,
        seed,
    };

    let format = req.response_format.to_lowercase();
    if !matches!(format.as_str(), "wav" | "pcm" | "pcm_s16le") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": {"message": "response_format must be wav or pcm", "type": "invalid_request_error"}})),
        ));
    }

    let started = Instant::now();

    // tiny-voca is CPU-bound and sync; run on blocking thread pool.
    // Tts is behind Arc<AppState> and speak takes &self, so this is safe.
    let state2 = state.clone();
    let text_owned = text.to_string();
    let voice_owned = voice.clone();

    let result = tokio::task::spawn_blocking(move || {
        state2.tts.speak(&text_owned, &voice_owned, &options)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": {"message": format!("task join: {e}"), "type": "server_error"}})),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": {"message": e, "type": "invalid_request_error"}})),
        )
    })?;

    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    info!(
        voice = %voice,
        speed = speed,
        chars = text.len(),
        samples = result.audio.len(),
        elapsed_ms = format!("{:.1}", elapsed_ms),
        model_ms = format!("{:.1}", result.model_time.as_secs_f64() * 1000.0),
        "synthesized"
    );

    let sample_rate = result.sample_rate;
    let audio = result.audio;

    match format.as_str() {
        "pcm" | "pcm_s16le" => {
            let mut pcm = Vec::with_capacity(audio.len() * 2);
            for &s in &audio {
                let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                pcm.extend_from_slice(&v.to_le_bytes());
            }
            let mut res = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "audio/pcm")],
                pcm,
            )
                .into_response();
            res.headers_mut().insert(
                header::HeaderName::from_static("x-sample-rate"),
                header::HeaderValue::from_str(&sample_rate.to_string()).unwrap(),
            );
            Ok(res)
        }
        _ => {
            // WAV (16-bit PCM)
            let wav = encode_wav_pcm16(&audio, sample_rate);
            Ok((StatusCode::OK, [(header::CONTENT_TYPE, "audio/wav")], wav).into_response())
        }
    }
}

fn encode_wav_pcm16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn resolve_data_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("VOCA_HOME") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("MODELS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    // Docker default
    let docker = Path::new("/models");
    if docker.join("tiny-voca.safetensors").exists() {
        return Ok(docker.to_path_buf());
    }
    // Local default
    let home = std::env::var_os("HOME").ok_or("HOME not set and VOCA_HOME / MODELS_DIR not provided")?;
    Ok(PathBuf::from(home).join(".local/share/voca"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "voca_server=info,tower_http=info".into()),
        )
        .init();

    let config = AppConfig::from_env();
    let data_dir = resolve_data_dir().unwrap_or_else(|e| {
        eprintln!("voca-server: cannot locate model data: {e}");
        std::process::exit(1);
    });

    let weights = data_dir.join("tiny-voca.safetensors");
    let phonemizer = data_dir.join("phonemizer");

    if !weights.exists() {
        eprintln!(
            "voca-server: model not found at {}\n\
             Download with:\n  curl -fsSL https://github.com/sultson/voca/releases/download/v0.1.0/voca-data.tar.gz | tar -xz -C {}",
            weights.display(),
            data_dir.display()
        );
        std::process::exit(1);
    }

    info!("loading model from {}", data_dir.display());

    // Limit BLAS / rayon threads for predictable latency
    std::env::set_var("VECLIB_MAXIMUM_THREADS", "1");
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .min(4);
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .ok();

    let tts = Tts::load(&weights, &phonemizer).unwrap_or_else(|e| {
        eprintln!("voca-server: failed to load model: {e}");
        std::process::exit(1);
    });

    info!(
        sample_rate = tts.model.config.sample_rate,
        voices = ?tts.model.config.speakers,
        "model ready"
    );

    let state = Arc::new(AppState { tts, config: config.clone() });

    let public = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models));

    let protected = Router::new()
        .route("/v1/audio/speech", post(speech))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let app = Router::new()
        .merge(public)
        .merge(protected)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .expect("invalid HOST:PORT");

    info!("listening on http://{addr}");
    if config.require_api_key {
        info!("API key authentication enabled");
    } else {
        warn!("API key authentication DISABLED");
    }

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutting down");
}
