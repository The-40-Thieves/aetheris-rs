//! Reverse proxy in front of local LLM inference engines (Ollama, LM Studio).
//!
//! Local apps point at `http://127.0.0.1:3030/ollama/...` (or `/lmstudio/...`)
//! instead of the engine directly. This proxy forwards the request unchanged,
//! streams the response straight back (so streaming latency is preserved), and
//! observes token-accounting metrics on the way through:
//!   * Ollama native (`/api/generate`, `/api/chat`) emits NDJSON; the terminal
//!     object (`done: true`) carries `eval_count` + `eval_duration` (ns), from
//!     which tokens/sec = eval_count * 1e9 / eval_duration.
//!   * LM Studio (OpenAI-compatible SSE) reports token *counts* in a final
//!     `usage` object but no durations, so tokens/sec is derived from measured
//!     wall-clock time.
//!
//! Observed metrics are logged to the `telemetry` table.
//!
//! The previous implementation intercepted and DISCARDED the request, returning
//! a fake `200` — which broke any real client pointed at it.

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use futures_util::StreamExt;
use reqwest::Client;
use std::sync::Arc;
use std::time::Instant;
use crate::database::Database;

#[derive(Clone)]
pub struct ProxyState {
    pub client: Client,
    pub db: Arc<Database>,
    pub ollama_base: String,
    pub lmstudio_base: String,
}

#[derive(Clone, Copy)]
enum Engine {
    Ollama,
    LmStudio,
}

/// Build the proxy router, reading upstream engine URLs from the environment
/// (`AETHERIS_OLLAMA_URL`, `AETHERIS_LMSTUDIO_URL`) with local defaults.
pub fn ai_router(db: Arc<Database>) -> Router {
    let ollama_base = std::env::var("AETHERIS_OLLAMA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let lmstudio_base = std::env::var("AETHERIS_LMSTUDIO_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:1234".to_string());
    ai_router_with(db, ollama_base, lmstudio_base)
}

/// Router with explicit upstream bases (used by tests to target a mock engine).
pub fn ai_router_with(db: Arc<Database>, ollama_base: String, lmstudio_base: String) -> Router {
    let state = ProxyState {
        client: Client::new(),
        db,
        ollama_base,
        lmstudio_base,
    };
    Router::new()
        // Axum 0.8 (matchit 0.8) wildcard capture syntax.
        .route("/ollama/{*path}", any(ollama_proxy))
        .route("/lmstudio/{*path}", any(lmstudio_proxy))
        .with_state(state)
}

async fn ollama_proxy(
    State(state): State<ProxyState>,
    req: Request,
) -> Result<Response, (StatusCode, String)> {
    let base = state.ollama_base.clone();
    proxy(state, req, Engine::Ollama, "/ollama", base).await
}

async fn lmstudio_proxy(
    State(state): State<ProxyState>,
    req: Request,
) -> Result<Response, (StatusCode, String)> {
    let base = state.lmstudio_base.clone();
    proxy(state, req, Engine::LmStudio, "/lmstudio", base).await
}

const MAX_REQUEST_BODY: usize = 32 * 1024 * 1024; // 32 MiB — generous for prompts

async fn proxy(
    state: ProxyState,
    req: Request,
    engine: Engine,
    prefix: &str,
    base: String,
) -> Result<Response, (StatusCode, String)> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path();
    let rest = path.strip_prefix(prefix).unwrap_or(path);
    let url = match uri.query() {
        Some(q) => format!("{base}{rest}?{q}"),
        None => format!("{base}{rest}"),
    };

    let req_headers = req.headers().clone();
    let body_bytes = axum::body::to_bytes(req.into_body(), MAX_REQUEST_BODY)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("reading request body: {e}")))?;

    // Rebuild the request for the upstream engine, preserving method/headers/body.
    let mut rb = state.client.request(method, &url).body(body_bytes.to_vec());
    for (k, v) in req_headers.iter() {
        // Drop Host so reqwest sets the correct upstream host header.
        if k != header::HOST {
            rb = rb.header(k, v);
        }
    }

    let started = Instant::now();
    let upstream = rb.send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("upstream engine at {base} unreachable: {e}"),
        )
    })?;

    let status = upstream.status();
    let resp_headers = upstream.headers().clone();
    let db = state.db.clone();

    // Stream the upstream body straight back to the caller, teeing complete
    // lines through the metric extractor as they pass. Only the terminal
    // accounting line is fully parsed; everything else forwards opaquely.
    let body_stream = async_stream::stream! {
        let mut bytes_stream = upstream.bytes_stream();
        let mut acc: Vec<u8> = Vec::new();
        let mut recorded = false;
        while let Some(item) = bytes_stream.next().await {
            match item {
                Ok(chunk) => {
                    consume_lines(engine, &mut acc, &chunk, &mut recorded, &db, started);
                    yield Ok::<axum::body::Bytes, std::io::Error>(chunk);
                }
                Err(e) => {
                    yield Err(std::io::Error::other(e));
                    return;
                }
            }
        }
        // Flush a final object that arrived without a trailing newline.
        if !recorded && !acc.is_empty() {
            let tail = String::from_utf8_lossy(&acc).into_owned();
            record_line(engine, &tail, &db, started);
        }
    };

    let mut builder = Response::builder().status(status);
    for (k, v) in resp_headers.iter() {
        // Let axum/hyper recompute framing headers for the re-streamed body.
        if k == header::CONTENT_LENGTH || k == header::TRANSFER_ENCODING {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from_stream(body_stream))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Feed a chunk into the line accumulator and record metrics for any completed
/// line. Sets `recorded` once a terminal accounting line has been seen.
fn consume_lines(
    engine: Engine,
    acc: &mut Vec<u8>,
    chunk: &[u8],
    recorded: &mut bool,
    db: &Arc<Database>,
    started: Instant,
) {
    acc.extend_from_slice(chunk);
    while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = acc.drain(..=pos).collect();
        let line = String::from_utf8_lossy(&line);
        if record_line(engine, &line, db, started) {
            *recorded = true;
        }
    }
    // Safety valve: never let a pathological no-newline stream grow unbounded.
    if acc.len() > MAX_REQUEST_BODY {
        acc.clear();
    }
}

/// Parse one line for terminal token-accounting and, if present, log it.
/// Returns true when this line was the accounting line.
fn record_line(engine: Engine, line: &str, db: &Arc<Database>, started: Instant) -> bool {
    match engine {
        Engine::Ollama => {
            if let Some(m) = parse_ollama_metrics(line) {
                let context = format!(
                    r#"{{"engine":"ollama","model":{},"eval_count":{},"prompt_eval_count":{}}}"#,
                    m.model_json, m.eval_count, m.prompt_eval_count,
                );
                log_tps(db, m.tokens_per_sec, &context);
                true
            } else {
                false
            }
        }
        Engine::LmStudio => {
            if let Some(completion_tokens) = parse_openai_completion_tokens(line) {
                let elapsed = started.elapsed().as_secs_f64();
                let tps = if elapsed > 0.0 {
                    completion_tokens as f64 / elapsed
                } else {
                    0.0
                };
                let context = format!(
                    r#"{{"engine":"lmstudio","completion_tokens":{completion_tokens},"elapsed_s":{elapsed:.3}}}"#
                );
                log_tps(db, tps, &context);
                true
            } else {
                false
            }
        }
    }
}

fn log_tps(db: &Arc<Database>, tps: f64, context: &str) {
    println!("[ai-proxy] tokens/sec={tps:.2} {context}");
    if let Err(e) = db.insert_metric("ai_tokens_per_sec", tps, context) {
        eprintln!("[ai-proxy] failed to record telemetry: {e}");
    }
}

struct OllamaMetrics {
    tokens_per_sec: f64,
    eval_count: u64,
    prompt_eval_count: u64,
    /// The model field re-serialized as a JSON value ("name" or null).
    model_json: String,
}

/// Extract tokens/sec from an Ollama terminal (`done: true`) NDJSON object.
/// Returns None for intermediate chunks, non-JSON, or when `eval_duration` is
/// missing/zero (we never divide by zero or invent a rate).
fn parse_ollama_metrics(line: &str) -> Option<OllamaMetrics> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("done").and_then(|d| d.as_bool()) != Some(true) {
        return None;
    }
    let eval_count = v.get("eval_count").and_then(|x| x.as_u64())?;
    let eval_duration = v.get("eval_duration").and_then(|x| x.as_u64())?;
    if eval_duration == 0 {
        return None;
    }
    let tokens_per_sec = eval_count as f64 * 1.0e9 / eval_duration as f64;
    let prompt_eval_count = v
        .get("prompt_eval_count")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let model_json = v
        .get("model")
        .map(|m| m.to_string())
        .unwrap_or_else(|| "null".to_string());
    Some(OllamaMetrics {
        tokens_per_sec,
        eval_count,
        prompt_eval_count,
        model_json,
    })
}

/// Extract `usage.completion_tokens` from an OpenAI-compatible SSE line
/// (`data: {..."usage":{...}}`). Returns None for content chunks (usage null),
/// the `[DONE]` sentinel, or non-usage lines.
fn parse_openai_completion_tokens(line: &str) -> Option<u64> {
    let line = line.trim();
    let data = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let usage = v.get("usage")?;
    if usage.is_null() {
        return None;
    }
    usage.get("completion_tokens").and_then(|x| x.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_router_builds_without_panicking() {
        // Regression test for the axum 0.8 wildcard route syntax (`{*path}`).
        let db = std::sync::Arc::new(
            crate::database::Database::new(std::path::PathBuf::from(":memory:"))
                .expect("in-memory db should init"),
        );
        let _router = ai_router(db);
    }

    #[test]
    fn parses_ollama_terminal_tokens_per_sec() {
        // Real sample from Ollama docs: eval_count 259, eval_duration 4232710000 ns.
        let line = r#"{"model":"llama3","done":true,"done_reason":"stop","eval_count":259,"eval_duration":4232710000,"prompt_eval_count":26,"prompt_eval_duration":130079000}"#;
        let m = parse_ollama_metrics(line).unwrap();
        // 259 * 1e9 / 4232710000 = ~61.19 tokens/sec
        assert!((m.tokens_per_sec - 61.19).abs() < 0.1, "tps was {}", m.tokens_per_sec);
        assert_eq!(m.eval_count, 259);
        assert_eq!(m.prompt_eval_count, 26);
        assert_eq!(m.model_json, "\"llama3\"");
    }

    #[test]
    fn ignores_intermediate_and_malformed_ollama_lines() {
        assert!(parse_ollama_metrics(r#"{"response":"hi","done":false}"#).is_none());
        assert!(parse_ollama_metrics("").is_none());
        assert!(parse_ollama_metrics("not json").is_none());
        // done:true but zero duration -> no divide-by-zero rate
        assert!(parse_ollama_metrics(r#"{"done":true,"eval_count":5,"eval_duration":0}"#).is_none());
    }

    #[test]
    fn parses_openai_usage_completion_tokens() {
        let line = r#"data: {"choices":[],"usage":{"prompt_tokens":26,"completion_tokens":282,"total_tokens":308}}"#;
        assert_eq!(parse_openai_completion_tokens(line), Some(282));
        // content chunk with null usage -> None
        assert!(parse_openai_completion_tokens(r#"data: {"choices":[{"delta":{"content":"hi"}}],"usage":null}"#).is_none());
        assert!(parse_openai_completion_tokens("data: [DONE]").is_none());
    }

    // --- End-to-end: mock upstream, proxy forwards + streams + records ---------

    async fn start_mock_ollama() -> String {
        use axum::routing::post;
        // Two NDJSON objects: a content chunk then the terminal accounting line.
        let app = Router::new().route(
            "/api/generate",
            post(|| async {
                let ndjson = "{\"model\":\"llama3\",\"response\":\"hi\",\"done\":false}\n\
                              {\"model\":\"llama3\",\"done\":true,\"done_reason\":\"stop\",\"eval_count\":100,\"eval_duration\":2000000000,\"prompt_eval_count\":10}\n";
                Response::builder()
                    .header("content-type", "application/x-ndjson")
                    .body(Body::from(ndjson))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn proxy_forwards_streams_and_records_metrics() {
        use tower::ServiceExt; // oneshot

        let mock = start_mock_ollama().await;
        let db = Arc::new(
            Database::new(std::path::PathBuf::from(":memory:")).unwrap(),
        );
        let app = ai_router_with(db.clone(), mock, "http://127.0.0.1:1".to_string());

        let req = Request::builder()
            .method("POST")
            .uri("/ollama/api/generate")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"llama3","prompt":"hi"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        // The upstream stream was forwarded verbatim (both NDJSON objects).
        assert!(text.contains("\"response\":\"hi\""), "forwarded body: {text}");
        assert!(text.contains("\"done\":true"), "forwarded body: {text}");

        // tokens/sec = 100 * 1e9 / 2e9 = 50.0 was recorded to telemetry.
        let conn = db.conn.lock().unwrap();
        let (count, val): (i64, f64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(value),0) FROM telemetry WHERE metric_type='ai_tokens_per_sec'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "exactly one tokens/sec sample recorded");
        assert!((val - 50.0).abs() < 0.01, "recorded tps was {val}");
    }
}
