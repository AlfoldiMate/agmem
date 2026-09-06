//! An embedding model behind an HTTP API (issue #120).
//!
//! Speaks the OpenAI `POST /embeddings` shape — `{"model", "input": [..]}`
//! in, `{"data": [{"index", "embedding"}]}` out — which is what OpenAI,
//! Voyage, Gemini's compatibility endpoint, Ollama, vLLM, LM Studio and
//! llama.cpp's own server all answer, so one backend covers the hosted
//! providers and a model served on another machine alike. The base URL and
//! the remote model name are configuration; the key rides `AGMEM_API_KEY`
//! and is never written anywhere.
//!
//! What this trades away is the point of the local runtime: the process now
//! needs the network on every write and every recall, each call is billed,
//! and the cosine bands the tools read were measured on the local models
//! and are **unmeasured** here (see [`ApiEmbedder::thresholds`]). The store
//! treats a remote model like any other vector space: its id and width are
//! recorded in `meta`, and switching to or from it is the ordinary model
//! migration (issue #138).

use std::time::Duration;

use agmem_core::dedup::Thresholds;
use serde::{Deserialize, Serialize};

use crate::{EmbedError, Embedder};

/// The name [`EmbedError::Backend`] reports for this backend.
const BACKEND: &str = "api";

/// What a store records for a remote model: the remote name under a prefix
/// that keeps it apart from the local ids, and says where the vectors came
/// from. Two endpoints serving the same model name are taken to be the same
/// space — an OpenAI-compatible proxy in front of OpenAI embeds identically.
fn model_id(model: &str) -> String {
    format!("api:{model}")
}

/// How long one request may take end to end. Generous: a 128-passage batch
/// through a slow provider is legitimately seconds, and the caller is on a
/// blocking thread already.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Attempts per request. A 429 or a 5xx from a hosted provider is routine
/// and clears in a moment; three tries with a short back-off absorb that
/// without hiding a real outage for long.
const ATTEMPTS: u32 = 3;

/// Embeds through an OpenAI-compatible embeddings endpoint.
pub struct ApiEmbedder {
    agent: ureq::Agent,
    /// `<base url>/embeddings`.
    endpoint: String,
    /// The `model` field of every request.
    model: String,
    /// What [`Embedder::model_id`] reports.
    id: String,
    /// The host part of the endpoint, for [`Embedder::accelerator`].
    host: String,
    /// Sent as `Authorization: Bearer …` when present. Local servers take
    /// none.
    key: Option<String>,
    /// Learnt from the first vector the endpoint returns.
    dim: usize,
}

impl std::fmt::Debug for ApiEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiEmbedder")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("dim", &self.dim)
            .field("key", &self.key.as_ref().map(|_| "…"))
            .finish()
    }
}

impl ApiEmbedder {
    /// Connect to `base_url` (`https://api.openai.com/v1`, or wherever
    /// `/embeddings` is served) and learn `model`'s width with one probe
    /// call — so a bad URL, a rejected key or an unknown model name fails
    /// here, at startup, where `doctor` and the log can name it, rather
    /// than on the first `remember`.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the endpoint cannot be reached, refuses
    /// the request, or answers with something other than one vector.
    pub fn new(base_url: &str, model: &str, key: Option<String>) -> Result<Self, EmbedError> {
        let base = base_url.trim_end_matches('/');
        let endpoint = format!("{base}/embeddings");
        let host = host_of(base).to_owned();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // Read the provider's error body instead of a bare status.
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        let mut embedder = Self {
            agent,
            endpoint,
            model: model.to_owned(),
            id: model_id(model),
            host,
            key,
            dim: 0,
        };
        let probe = embedder.request(&["agmem".to_owned()])?;
        let dim = probe.first().map_or(0, Vec::len);
        if dim == 0 {
            return Err(failed(format!(
                "{model} at {} answered the probe with no vector",
                embedder.endpoint
            )));
        }
        embedder.dim = dim;
        tracing::info!(
            model = embedder.id,
            dim,
            endpoint = embedder.endpoint,
            "api embedder ready"
        );
        Ok(embedder)
    }

    /// One `/embeddings` call for `texts`, retried on the transient
    /// answers, vectors normalised to unit length and in input order.
    fn request(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let body = Request {
            model: &self.model,
            input: texts,
        };
        let mut last = String::new();
        for attempt in 1..=ATTEMPTS {
            match self.once(&body) {
                Ok(vectors) => return Ok(vectors),
                Err(Outcome::Final(message)) => return Err(failed(message)),
                Err(Outcome::Transient(message)) => {
                    tracing::warn!(attempt, %message, "api embed request failed; retrying");
                    last = message;
                    std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
                }
            }
        }
        Err(failed(format!("after {ATTEMPTS} attempts: {last}")))
    }

    fn once(&self, body: &Request<'_>) -> Result<Vec<Vec<f32>>, Outcome> {
        let mut request = self.agent.post(&self.endpoint);
        if let Some(key) = &self.key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = request
            .send_json(body)
            .map_err(|e| Outcome::Transient(format!("{}: {e}", self.endpoint)))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.body_mut().read_to_string().unwrap_or_default();
            let message = format!(
                "{} answered {status}: {}",
                self.endpoint,
                error_message(&text)
            );
            return Err(if status == 429 || status >= 500 {
                Outcome::Transient(message)
            } else {
                Outcome::Final(message)
            });
        }
        let parsed: Response = response.body_mut().read_json().map_err(|e| {
            Outcome::Final(format!("{} answered malformed JSON: {e}", self.endpoint))
        })?;
        let mut data = parsed.data;
        if data.len() != body.input.len() {
            return Err(Outcome::Final(format!(
                "{} returned {} vectors for {} inputs",
                self.endpoint,
                data.len(),
                body.input.len()
            )));
        }
        data.sort_by_key(|datum| datum.index);
        Ok(data
            .into_iter()
            .map(|datum| normalise(datum.embedding))
            .collect())
    }
}

/// Why one attempt did not produce vectors, and whether another might.
enum Outcome {
    /// The provider or the network hiccupped; try again.
    Transient(String),
    /// The request is wrong as asked: bad key, unknown model, bad shape.
    Final(String),
}

fn failed(message: String) -> EmbedError {
    EmbedError::Backend {
        backend: BACKEND,
        message,
    }
}

/// The host of a URL, for the report line: `https://api.openai.com/v1` →
/// `api.openai.com`.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// The provider's own words from an error body — OpenAI's
/// `{"error": {"message": …}}` — else the body itself, kept short.
fn error_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct Wrapped {
        error: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        message: String,
    }
    let text = serde_json::from_str::<Wrapped>(body)
        .map_or_else(|_| body.trim().to_owned(), |wrapped| wrapped.error.message);
    if text.is_empty() {
        return "no body".to_owned();
    }
    text.chars().take(300).collect()
}

/// Unit length, so cosine reads as the dot product the store's index expects
/// whichever provider answered — OpenAI normalises, Ollama does not.
fn normalise(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut vector {
            *x /= norm;
        }
    }
    vector
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Datum>,
}

#[derive(Deserialize)]
struct Datum {
    index: usize,
    embedding: Vec<f32>,
}

impl Embedder for ApiEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &str {
        &self.id
    }

    /// A hosted model has no revision a client can see: the store keys on
    /// the id and the width, and a provider that silently retrains under
    /// the same name is a risk this backend cannot close.
    fn revision(&self) -> Option<&str> {
        None
    }

    /// **Unmeasured.** The bands were calibrated on the local models
    /// (`docs/eval/embed-models.md`); no remote model has been through the
    /// harness. EmbeddingGemma's table is the one carried because current
    /// hosted models score unrelated pairs near zero as Gemma does, where
    /// bge's high floor would silence recall's abstention entirely. Until
    /// measured, expect the dedup gate and the abstention floor to be
    /// approximate.
    fn thresholds(&self) -> Thresholds {
        Thresholds::GEMMA_300M
    }

    /// Where the model runs: not here. The endpoint's host, so the doctor
    /// line reads `api:text-embedding-3-small (1536d, api.openai.com)`.
    fn accelerator(&self) -> &str {
        &self.host
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        self.request(passages)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        let mut vectors = self.request(&[query.to_owned()])?;
        vectors
            .pop()
            .ok_or_else(|| failed("the endpoint answered a query with no vector".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// One request as the mock saw it.
    struct Seen {
        authorization: Option<String>,
        body: serde_json::Value,
    }

    /// A one-thread OpenAI-shaped endpoint on localhost. Each queued reply
    /// answers one request, in order; the requests are recorded.
    struct Mock {
        url: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Mock {
        fn serve(replies: Vec<(u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let url = format!("http://{}/v1", listener.local_addr().expect("addr"));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&seen);
            std::thread::spawn(move || {
                for (status, reply) in replies {
                    let (mut stream, _) = listener.accept().expect("accept");
                    let (headers, body) = read_request(&mut stream);
                    let authorization = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("authorization: "))
                        .map(str::to_owned);
                    log.lock().unwrap().push(Seen {
                        authorization,
                        body: serde_json::from_str(&body).expect("json body"),
                    });
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    stream.write_all(response.as_bytes()).expect("write");
                }
            });
            Self { url, seen }
        }

        fn seen(&self) -> Vec<Seen> {
            std::mem::take(&mut *self.seen.lock().unwrap())
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).expect("read");
            raw.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(split) = text.find("\r\n\r\n") {
                let headers = text[..split].to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                let have = raw.len() - split - 4;
                if have >= length {
                    let body = String::from_utf8_lossy(&raw[split + 4..]).into_owned();
                    return (headers, body);
                }
            }
            assert!(n > 0, "connection closed mid-request");
        }
    }

    fn vectors(rows: &[(usize, &[f32])]) -> String {
        let data: Vec<serde_json::Value> = rows
            .iter()
            .map(|(index, embedding)| {
                serde_json::json!({ "index": index, "embedding": embedding, "object": "embedding" })
            })
            .collect();
        serde_json::json!({ "object": "list", "data": data, "model": "mock" }).to_string()
    }

    #[test]
    fn probes_once_learns_the_width_and_sends_the_key() {
        let mock = Mock::serve(vec![
            (200, vectors(&[(0, &[3.0, 4.0])])),
            (200, vectors(&[(1, &[0.0, 1.0]), (0, &[1.0, 0.0])])),
            (200, vectors(&[(0, &[0.6, 0.8])])),
        ]);

        let embedder =
            ApiEmbedder::new(&mock.url, "mock-embed", Some("sk-test".to_owned())).expect("new");
        assert_eq!(embedder.dim(), 2);
        assert_eq!(embedder.model_id(), "api:mock-embed");
        assert_eq!(embedder.revision(), None);
        assert!(embedder.accelerator().starts_with("127.0.0.1:"));
        assert!(
            !format!("{embedder:?}").contains("sk-test"),
            "the key never prints"
        );

        let passages = embedder
            .embed_passages(&["a".to_owned(), "b".to_owned()])
            .expect("passages");
        assert_eq!(
            passages,
            vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            "vectors come back in input order whatever order the provider lists them"
        );
        let query = embedder.embed_query("q").expect("query");
        assert!((query[0] - 0.6).abs() < 1e-6 && (query[1] - 0.8).abs() < 1e-6);

        let seen = mock.seen();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].authorization.as_deref(), Some("bearer sk-test"));
        assert_eq!(seen[0].body["model"], "mock-embed");
        assert_eq!(seen[0].body["input"], serde_json::json!(["agmem"]));
        assert_eq!(seen[1].body["input"], serde_json::json!(["a", "b"]));
        assert_eq!(seen[2].body["input"], serde_json::json!(["q"]));
    }

    #[test]
    fn the_probe_normalises_and_a_keyless_endpoint_gets_no_header() {
        let mock = Mock::serve(vec![(200, vectors(&[(0, &[3.0, 4.0])]))]);
        let embedder = ApiEmbedder::new(&format!("{}/", mock.url), "m", None).expect("new");
        assert_eq!(embedder.dim(), 2);
        assert_eq!(mock.seen()[0].authorization, None, "no key, no header");
        // A trailing slash on the base URL does not double up.
        assert!(embedder.endpoint.ends_with("/v1/embeddings"));
    }

    #[test]
    fn a_rejected_key_fails_at_construction_with_the_providers_words() {
        let mock = Mock::serve(vec![(
            401,
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#
                .to_owned(),
        )]);
        let err = ApiEmbedder::new(&mock.url, "m", Some("bad".to_owned())).expect_err("refused");
        let text = err.to_string();
        assert!(text.contains("401"), "{text}");
        assert!(text.contains("Incorrect API key provided"), "{text}");
        assert_eq!(mock.seen().len(), 1, "a 4xx is final: no retry");
    }

    #[test]
    fn a_429_is_retried_and_a_wrong_count_is_final() {
        let mock = Mock::serve(vec![
            (429, "slow down".to_owned()),
            (200, vectors(&[(0, &[1.0])])),
            (200, vectors(&[(0, &[1.0])])),
        ]);
        let embedder = ApiEmbedder::new(&mock.url, "m", None).expect("retried past the 429");
        let err = embedder
            .embed_passages(&["a".to_owned(), "b".to_owned()])
            .expect_err("one vector for two inputs is refused");
        assert!(err.to_string().contains("1 vectors for 2 inputs"), "{err}");
        assert_eq!(mock.seen().len(), 3);
    }

    #[test]
    fn an_unreachable_endpoint_is_an_error_not_a_panic() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        drop(listener);
        let err = ApiEmbedder::new(&url, "m", None).expect_err("refused");
        assert!(err.to_string().contains("attempts"), "{err}");
    }

    #[test]
    fn hosts_and_error_bodies_read_as_expected() {
        assert_eq!(host_of("https://api.openai.com/v1"), "api.openai.com");
        assert_eq!(host_of("http://localhost:11434/v1/"), "localhost:11434");
        assert_eq!(host_of("nohost"), "nohost");
        assert_eq!(error_message(""), "no body");
        assert_eq!(error_message("plain text\n"), "plain text");
        assert_eq!(error_message(r#"{"error":{"message":"m"}}"#), "m");
    }
}
