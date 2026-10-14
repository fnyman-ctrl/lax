//! Calls to an OpenAI-compatible server: chat, embeddings and the model list
//! (ADR 0004, ADR 0007, GUARDRAILS §3).
//!
//! Only local servers can be used for now. Remote servers arrive with the
//! consent screen (#16), the Keychain (#17), redaction (#18) and the outbox
//! log (#19); until then `ModelClient::new` refuses them.
//!
//! Errors never carry what a server sent back: a response body can echo the
//! prompt, which can hold captured content (GUARDRAILS §6).

use std::fmt;
use std::time::{Duration, Instant};

use reqwest::{Client, Response, StatusCode, redirect};
use serde::{Deserialize, Serialize};

use super::endpoint::{Endpoint, Locality};
use super::sse;
use crate::logging::{self, Field};

/// How long to wait for a server to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Local models can be slow to answer, especially the first time.
const CHAT_TIMEOUT: Duration = Duration::from_secs(180);
const EMBEDDINGS_TIMEOUT: Duration = Duration::from_secs(60);
const MODELS_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest response Lax reads. Real answers are far smaller.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Waits before the second and third attempt.
const BACKOFF: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1500)];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// What can go wrong, without anything the server sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelError {
    /// A remote server, which needs consent first (#16).
    RemoteNotAllowed,
    /// Nothing answered at the address.
    Unreachable,
    Timeout,
    /// The server answered with this HTTP status.
    Status(u16),
    /// The answer wasn't what an OpenAI-compatible server sends.
    InvalidResponse,
    TooLarge,
    /// The model used up its token budget before giving an answer (reasoning
    /// models can spend it all on thinking).
    OutOfRoom,
}

impl ModelError {
    /// The server is unreachable, slow or erroring, or not allowed: not the
    /// fault of what was asked, so worth waiting a while before asking again.
    pub fn unavailable(&self) -> bool {
        matches!(
            self,
            ModelError::Unreachable
                | ModelError::Timeout
                | ModelError::Status(_)
                | ModelError::RemoteNotAllowed
        )
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::RemoteNotAllowed => {
                f.write_str("Only model servers on this Mac can be used for now.")
            }
            ModelError::Unreachable => f.write_str("No model server answered at that address."),
            ModelError::Timeout => f.write_str("The model server took too long to answer."),
            ModelError::Status(code) => write!(f, "The model server answered with error {code}."),
            ModelError::InvalidResponse => {
                f.write_str("The model server's answer wasn't in the expected format.")
            }
            ModelError::TooLarge => f.write_str("The model server's answer was too large."),
            ModelError::OutOfRoom => {
                f.write_str("The model ran out of room before it could answer.")
            }
        }
    }
}

impl std::error::Error for ModelError {}

pub struct ModelClient {
    http: Client,
    endpoint: Endpoint,
    backoff: [Duration; 2],
    retries: bool,
    /// Let models that reason before answering do so. Off for questions the
    /// user waits on: reasoning makes a small local model about ten times
    /// slower.
    reasoning: bool,
    /// Sent with chat requests when set; otherwise the server's default.
    temperature: Option<f32>,
}

impl ModelClient {
    /// A client for a local server. Remote servers are refused until they
    /// can be enabled with consent (#16).
    pub fn new(endpoint: Endpoint) -> Result<Self, ModelError> {
        if endpoint.locality() != Locality::Local {
            return Err(ModelError::RemoteNotAllowed);
        }
        let http = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // A loopback server needs no proxy, and a proxy would see the
            // request.
            .no_proxy()
            // Never follow a redirect to another server.
            .redirect(redirect::Policy::none())
            .user_agent(concat!("Lax/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ModelError::Unreachable)?;
        Ok(ModelClient {
            http,
            endpoint,
            backoff: BACKOFF,
            retries: true,
            reasoning: true,
            temperature: None,
        })
    }

    /// The models the server offers, by id.
    pub async fn models(&self) -> Result<Vec<String>, ModelError> {
        #[derive(Deserialize)]
        struct Models {
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            id: String,
        }
        let models: Models = self
            .call("model.models", "models", None::<&()>, MODELS_TIMEOUT)
            .await?;
        Ok(models.data.into_iter().map(|m| m.id).collect())
    }

    /// The assistant's reply to `messages`.
    pub async fn chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: Option<u32>,
    ) -> Result<String, ModelError> {
        let started = Instant::now();
        let request = self.chat_request(model, messages, max_tokens, false);
        let (opened, attempts) = self.open_chat(request).await;
        let result = match opened {
            Ok(response) => read(response).await,
            Err(error) => Err(error),
        };
        log_call("model.chat", &result, attempts, started, &[]);
        let reply: ChatReply =
            serde_json::from_slice(&result?).map_err(|_| ModelError::InvalidResponse)?;
        reply.content()
    }

    /// Like [`chat`](Self::chat), but the server sends the reply as it is
    /// written (`"stream": true`, server-sent events), and `on_text` gets
    /// each new piece of it. Returns the whole reply, which the caller must
    /// still validate: the pieces are the model's raw output.
    ///
    /// The timeout, retries and fallback are the same as for `chat`, and
    /// dropping the future (as `Pause::unless_paused` does) closes the
    /// connection, so the server can stop. Logs carry counts and timings
    /// only.
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        max_tokens: Option<u32>,
        mut on_text: impl FnMut(&str),
    ) -> Result<String, ModelError> {
        let started = Instant::now();
        let request = self.chat_request(model, messages, max_tokens, true);
        let (opened, attempts) = self.open_chat(request).await;
        let mut first: Option<Duration> = None;
        let mut pieces: u64 = 0;
        let result = match opened {
            Ok(response) => {
                read_stream(response, |piece| {
                    first.get_or_insert_with(|| started.elapsed());
                    pieces += 1;
                    on_text(piece);
                })
                .await
            }
            Err(error) => Err(error),
        };
        let mut fields = vec![Field::Count("pieces", pieces)];
        if let Some(first) = first {
            fields.push(Field::Duration("first_piece", first));
        }
        log_call("model.chat_stream", &result, attempts, started, &fields);
        result
    }

    fn chat_request<'a>(
        &self,
        model: &'a str,
        messages: &'a [ChatMessage],
        max_tokens: Option<u32>,
        stream: bool,
    ) -> ChatRequest<'a> {
        ChatRequest {
            model,
            messages,
            max_tokens,
            reasoning_effort: (!self.reasoning).then_some("none"),
            stream,
            temperature: self.temperature,
        }
    }

    /// Sends a chat request. A server that doesn't take
    /// `reasoning_effort: "none"` refuses it; then it is sent again without,
    /// and the model may reason.
    async fn open_chat(
        &self,
        mut request: ChatRequest<'_>,
    ) -> (Result<Response, ModelError>, usize) {
        let path = "chat/completions";
        let (opened, attempts) = self.open(path, Some(&request), CHAT_TIMEOUT).await;
        if request.reasoning_effort.is_none()
            || !matches!(opened, Err(ModelError::Status(400 | 422)))
        {
            return (opened, attempts);
        }
        request.reasoning_effort = None;
        let (opened, more) = self.open(path, Some(&request), CHAT_TIMEOUT).await;
        (opened, attempts + more)
    }

    /// One embedding per input, in the order of `inputs`.
    pub async fn embeddings(
        &self,
        model: &str,
        inputs: &[String],
    ) -> Result<Vec<Vec<f32>>, ModelError> {
        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            input: &'a [String],
        }
        #[derive(Deserialize)]
        struct Reply {
            data: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        let request = Request {
            model,
            input: inputs,
        };
        let reply: Reply = self
            .call(
                "model.embeddings",
                "embeddings",
                Some(&request),
                EMBEDDINGS_TIMEOUT,
            )
            .await?;
        let mut items = reply.data;
        items.sort_by_key(|item| item.index);
        let in_order = items.iter().enumerate().all(|(i, item)| item.index == i);
        if items.len() != inputs.len() || !in_order {
            return Err(ModelError::InvalidResponse);
        }
        Ok(items.into_iter().map(|item| item.embedding).collect())
    }

    /// Sends `body` (or a GET without one) to `path`, retrying when the
    /// server is busy or briefly unreachable, and parses the answer.
    async fn call<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        event: &'static str,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T, ModelError> {
        let started = Instant::now();
        let (opened, attempts) = self.open(path, body, timeout).await;
        let result = match opened {
            Ok(response) => read(response).await,
            Err(error) => Err(error),
        };
        log_call(event, &result, attempts, started, &[]);
        let bytes = result?;
        serde_json::from_slice(&bytes).map_err(|_| ModelError::InvalidResponse)
    }

    /// Sends `body` (or a GET without one) to `path`, retrying when the
    /// server is busy or briefly unreachable. Returns a response that
    /// answered 200, whose body is still to be read, and how many attempts
    /// it took.
    async fn open<B: Serialize>(
        &self,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> (Result<Response, ModelError>, usize) {
        let url = self.endpoint.url(path);
        let mut attempts = 0;
        loop {
            attempts += 1;
            let request = match body {
                Some(body) => self.http.post(url.clone()).json(body),
                None => self.http.get(url.clone()),
            };
            // Only retry what didn't reach the server (a failed connection)
            // or what the server turned down without handling (busy, down).
            // Anything else may have been received, and sending it again
            // would send its content again. The timeout covers reading the
            // body too.
            let (outcome, retry) = match request.timeout(timeout).send().await {
                Ok(response) if response.status() == StatusCode::OK => (Ok(response), false),
                Ok(response) => {
                    let code = response.status().as_u16();
                    (
                        Err(ModelError::Status(code)),
                        matches!(code, 429 | 502 | 503 | 504),
                    )
                }
                Err(error) if error.is_timeout() => (Err(ModelError::Timeout), false),
                Err(error) => (Err(ModelError::Unreachable), error.is_connect()),
            };
            match self.backoff.get(attempts - 1) {
                Some(wait) if retry && self.retries => tokio::time::sleep(*wait).await,
                _ => return (outcome, attempts),
            }
        }
    }

    /// The same client, asking models not to reason before answering
    /// (`reasoning_effort: "none"`, which servers without reasoning ignore).
    pub fn without_reasoning(mut self) -> Self {
        self.reasoning = false;
        self
    }

    /// The same client, asking for this sampling temperature: low for text
    /// that must follow rules, such as a draft.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// The same client without retries, for quick checks such as looking
    /// for servers that may not be running.
    pub fn without_retries(mut self) -> Self {
        self.backoff = [Duration::ZERO; 2];
        self.retries = false;
        self
    }

    #[cfg(test)]
    fn with_backoff(mut self, backoff: [Duration; 2]) -> Self {
        self.backoff = backoff;
        self
    }
}

/// Logs a call to the server: its status (0 if none came back), attempts,
/// time, and `extra`. Never anything the server sent.
fn log_call<T>(
    event: &'static str,
    result: &Result<T, ModelError>,
    attempts: usize,
    started: Instant,
    extra: &[Field],
) {
    let status = match result {
        Ok(_) => 200,
        Err(ModelError::Status(code)) => u64::from(*code),
        Err(_) => 0,
    };
    let mut fields = vec![
        Field::Count("status", status),
        Field::Count("attempts", attempts as u64),
        Field::Duration("elapsed", started.elapsed()),
    ];
    fields.extend_from_slice(extra);
    logging::info(event, &fields);
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize)]
struct ChatReply {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatContent,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatContent {
    content: Option<String>,
}

impl ChatReply {
    fn content(self) -> Result<String, ModelError> {
        let choice = self
            .choices
            .into_iter()
            .next()
            .ok_or(ModelError::InvalidResponse)?;
        content_or_why(choice.message.content, choice.finish_reason.as_deref())
    }
}

/// The reply, or why there is none.
fn content_or_why(content: Option<String>, finish: Option<&str>) -> Result<String, ModelError> {
    match content {
        Some(content) if !content.trim().is_empty() => Ok(content),
        _ if finish == Some("length") => Err(ModelError::OutOfRoom),
        _ => Err(ModelError::InvalidResponse),
    }
}

/// One event of a streamed reply.
#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    /// Some servers report a failure mid-stream as an event of its own.
    #[serde(default)]
    error: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// Only the reply's content is read: a reasoning model's thinking
/// (`reasoning_content`) is never passed on.
#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

/// The body of a successful response, at most `MAX_RESPONSE_BYTES`.
async fn read(mut response: Response) -> Result<Vec<u8>, ModelError> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err(ModelError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(error) if error.is_timeout() => return Err(ModelError::Timeout),
            Err(_) => return Err(ModelError::Unreachable),
        }
    }
}

/// A streamed reply, read as it arrives, at most `MAX_RESPONSE_BYTES` in
/// all. A server that ignores `stream` and answers with one JSON body (by
/// whatever content type) is read as such, and `on_text` gets the whole
/// reply at once.
async fn read_stream(
    mut response: Response,
    mut on_text: impl FnMut(&str),
) -> Result<String, ModelError> {
    let mut decoder = sse::Decoder::default();
    let mut reply = StreamedReply::default();
    // The body, kept only until the first event shows it is a stream.
    let mut body = Vec::new();
    let mut streaming = false;
    let mut read_bytes = 0;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if error.is_timeout() => return Err(ModelError::Timeout),
            Err(_) => return Err(ModelError::Unreachable),
        };
        read_bytes += chunk.len();
        if read_bytes > MAX_RESPONSE_BYTES {
            return Err(ModelError::TooLarge);
        }
        if !streaming {
            body.extend_from_slice(&chunk);
        }
        let events = decoder
            .push(&chunk)
            .map_err(|_| ModelError::InvalidResponse)?;
        for data in events {
            streaming = true;
            if reply.event(&data, &mut on_text)? {
                return reply.finish();
            }
        }
        if streaming {
            body = Vec::new();
        }
    }
    // The body ended without "[DONE]", which some servers leave out.
    if let Some(data) = decoder.finish().map_err(|_| ModelError::InvalidResponse)? {
        streaming = true;
        reply.event(&data, &mut on_text)?;
    }
    if streaming {
        return reply.finish();
    }
    let whole: ChatReply =
        serde_json::from_slice(&body).map_err(|_| ModelError::InvalidResponse)?;
    let content = whole.content()?;
    on_text(&content);
    Ok(content)
}

/// A streamed reply as far as it has come.
#[derive(Default)]
struct StreamedReply {
    content: String,
    finish_reason: Option<String>,
}

impl StreamedReply {
    /// Takes one event's data. `Ok(true)` at the end of the stream.
    fn event(&mut self, data: &str, on_text: &mut impl FnMut(&str)) -> Result<bool, ModelError> {
        match data.trim() {
            "[DONE]" => return Ok(true),
            // A keep-alive.
            "" => return Ok(false),
            _ => {}
        }

        let chunk: StreamChunk =
            serde_json::from_str(data).map_err(|_| ModelError::InvalidResponse)?;
        if chunk.error.is_some() {
            return Err(ModelError::InvalidResponse);
        }
        // Only the first choice, as `chat` reads only the first.
        for choice in chunk.choices.into_iter().filter(|c| c.index == 0) {
            if let Some(piece) = choice.delta.and_then(|d| d.content)
                && !piece.is_empty()
            {
                self.content.push_str(&piece);
                on_text(&piece);
            }
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }
        }
        Ok(false)
    }

    fn finish(self) -> Result<String, ModelError> {
        content_or_why(Some(self.content), self.finish_reason.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// What the mock server answers to one request.
    enum Reply {
        /// A status and a JSON body.
        Json(u16, &'static str),
        /// Server-sent events, written in `parts` with a short wait between
        /// them. With `hold`, the connection is then kept open with comments
        /// until the client closes it.
        Stream { parts: Vec<Vec<u8>>, hold: bool },
    }

    /// A tiny HTTP server on 127.0.0.1 that answers with `replies`, one per
    /// request, and records each request's first line and body.
    struct MockServer {
        address: String,
        requests: Arc<Mutex<Vec<(String, String)>>>,
        /// The client closed a held stream.
        closed: Arc<AtomicBool>,
    }

    impl MockServer {
        fn start(replies: Vec<(u16, &'static str)>) -> Self {
            Self::start_with(
                replies
                    .into_iter()
                    .map(|(status, body)| Reply::Json(status, body))
                    .collect(),
            )
        }

        fn start_with(replies: Vec<Reply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = format!("http://{}/v1", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            let closed = Arc::new(AtomicBool::new(false));
            let closed_seen = closed.clone();
            thread::spawn(move || {
                for reply in replies {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    let mut request_body = vec![0; length];
                    reader.read_exact(&mut request_body).unwrap();
                    seen.lock().unwrap().push((
                        first.trim().to_string(),
                        String::from_utf8(request_body).unwrap(),
                    ));
                    match reply {
                        Reply::Json(status, body) => {
                            let reply = format!(
                                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            stream.write_all(reply.as_bytes()).unwrap();
                        }
                        Reply::Stream { parts, hold } => {
                            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
                            stream.write_all(head.as_bytes()).unwrap();
                            for part in parts {
                                stream.write_all(&part).unwrap();
                                stream.flush().unwrap();
                                thread::sleep(Duration::from_millis(5));
                            }
                            // Until the client closes the connection.
                            while hold
                                && stream.write_all(b": waiting\n\n").is_ok()
                                && stream.flush().is_ok()
                            {
                                thread::sleep(Duration::from_millis(50));
                            }
                            if hold {
                                closed_seen.store(true, Ordering::SeqCst);
                            }
                        }
                    }
                }
            });
            MockServer {
                address,
                requests,
                closed,
            }
        }

        fn client(&self) -> ModelClient {
            ModelClient::new(Endpoint::parse(&self.address).unwrap())
                .unwrap()
                .with_backoff([Duration::from_millis(1); 2])
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: text.to_string(),
        }
    }

    #[tokio::test]
    async fn chat_sends_the_messages_and_returns_the_reply() {
        let server = MockServer::start(vec![(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"Hej Anna"}}]}"#,
        )]);
        let reply = server
            .client()
            .chat("gemma", &[user("Say hello")], Some(20))
            .await
            .unwrap();
        assert_eq!(reply, "Hej Anna");
        let requests = server.requests();
        assert!(requests[0].0.starts_with("POST /v1/chat/completions"));
        let sent: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
        assert_eq!(sent["model"], "gemma");
        assert_eq!(sent["messages"][0]["role"], "user");
        assert_eq!(sent["messages"][0]["content"], "Say hello");
        assert_eq!(sent["max_tokens"], 20);
        assert_eq!(sent["stream"], false);
        // Models may reason unless told not to.
        assert!(sent.get("reasoning_effort").is_none());
        // The server's own temperature unless one is asked for.
        assert!(sent.get("temperature").is_none());
    }

    #[tokio::test]
    async fn a_temperature_is_sent_when_asked_for() {
        let server = MockServer::start(vec![(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"Hej"}}]}"#,
        )]);
        server
            .client()
            .with_temperature(0.1)
            .chat("gemma", &[user("Say hello")], Some(20))
            .await
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&server.requests()[0].1).unwrap();
        let temperature = sent["temperature"].as_f64().unwrap();
        assert!((temperature - 0.1).abs() < 1e-6);
    }

    #[tokio::test]
    async fn a_server_that_refuses_no_reasoning_is_asked_again() {
        let server = MockServer::start(vec![
            (400, r#"{"error":"unknown reasoning_effort"}"#),
            (
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"Hej"}}]}"#,
            ),
        ]);
        let reply = server
            .client()
            .without_reasoning()
            .chat("gemma", &[user("Say hello")], Some(20))
            .await
            .unwrap();
        assert_eq!(reply, "Hej");
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let second: serde_json::Value = serde_json::from_str(&requests[1].1).unwrap();
        assert!(second.get("reasoning_effort").is_none());
    }

    #[tokio::test]
    async fn a_client_without_reasoning_says_so() {
        let server = MockServer::start(vec![(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"Hej"}}]}"#,
        )]);
        server
            .client()
            .without_reasoning()
            .chat("gemma", &[user("Say hello")], Some(20))
            .await
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&server.requests()[0].1).unwrap();
        assert_eq!(sent["reasoning_effort"], "none");
    }

    #[tokio::test]
    async fn embeddings_come_back_in_input_order() {
        let server = MockServer::start(vec![(
            200,
            r#"{"data":[{"index":1,"embedding":[0.5,0.5]},{"index":0,"embedding":[1.0,0.0]}]}"#,
        )]);
        let vectors = server
            .client()
            .embeddings("nomic", &["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.5, 0.5]]);
    }

    #[tokio::test]
    async fn embeddings_must_match_the_inputs() {
        let server = MockServer::start(vec![(200, r#"{"data":[{"index":0,"embedding":[1.0]}]}"#)]);
        let result = server
            .client()
            .embeddings("nomic", &["a".to_string(), "b".to_string()])
            .await;
        assert_eq!(result, Err(ModelError::InvalidResponse));
    }

    #[tokio::test]
    async fn models_are_listed_by_id() {
        let server = MockServer::start(vec![(
            200,
            r#"{"object":"list","data":[{"id":"gemma-4-e4b"},{"id":"qwen3.5-9b"}]}"#,
        )]);
        let models = server.client().models().await.unwrap();
        assert_eq!(models, ["gemma-4-e4b", "qwen3.5-9b"]);
        assert!(server.requests()[0].0.starts_with("GET /v1/models"));
    }

    #[tokio::test]
    async fn running_out_of_room_is_told_apart() {
        let server = MockServer::start(vec![(
            200,
            r#"{"choices":[{"message":{"content":"","reasoning_content":"Let me think"},"finish_reason":"length"}]}"#,
        )]);
        let reply = server.client().chat("qwen", &[user("x")], Some(10)).await;
        assert_eq!(reply, Err(ModelError::OutOfRoom));
    }

    #[tokio::test]
    async fn busy_servers_are_retried() {
        let server = MockServer::start(vec![
            (503, "{}"),
            (200, r#"{"choices":[{"message":{"content":"ok"}}]}"#),
        ]);
        let reply = server.client().chat("m", &[user("x")], None).await;
        assert_eq!(reply.as_deref(), Ok("ok"));
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn client_errors_are_not_retried() {
        let server = MockServer::start(vec![(400, "{}"), (200, "{}")]);
        let reply = server.client().chat("m", &[user("x")], None).await;
        assert_eq!(reply, Err(ModelError::Status(400)));
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn errors_never_carry_what_the_server_sent() {
        let server = MockServer::start(vec![(
            500,
            r#"{"error":"Anna Example's diagnosis was in the prompt"}"#,
        )]);
        let error = server
            .client()
            .chat("m", &[user("x")], None)
            .await
            .unwrap_err();
        assert_eq!(error, ModelError::Status(500));
        assert!(!error.to_string().contains("Anna"));
        // A 500 may mean the server handled the request and failed, so it
        // isn't sent again.
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn answers_in_another_shape_are_refused() {
        let server = MockServer::start(vec![
            (200, "not json"),
            (200, r#"{"choices":[]}"#),
            (200, r#"{"choices":[{"message":{"content":null}}]}"#),
        ]);
        let client = server.client();
        for _ in 0..3 {
            assert_eq!(
                client.chat("m", &[user("x")], None).await,
                Err(ModelError::InvalidResponse)
            );
        }
    }

    #[tokio::test]
    async fn nothing_listening_is_unreachable() {
        // Bind and drop, so the port is very likely closed.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client =
            ModelClient::new(Endpoint::parse(&format!("http://127.0.0.1:{port}/v1")).unwrap())
                .unwrap()
                .with_backoff([Duration::from_millis(1); 2]);
        assert_eq!(client.models().await, Err(ModelError::Unreachable));
    }

    /// Talks to a real model server on this Mac. Not run by default; run with
    ///
    /// ```sh
    /// LAX_LIVE_MODEL_URL=http://localhost:1234/v1 \
    /// LAX_LIVE_CHAT_MODEL=google/gemma-4-e4b \
    /// LAX_LIVE_EMBEDDING_MODEL=text-embedding-bge-m3 \
    /// cargo test live_local_server -- --ignored
    /// ```
    ///
    /// The prompts are fixed and synthetic; nothing captured is sent.
    #[tokio::test]
    #[ignore = "needs a model server running on this Mac"]
    async fn live_local_server() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let client =
            ModelClient::new(Endpoint::parse(&env("LAX_LIVE_MODEL_URL")).unwrap()).unwrap();
        let chat_model = env("LAX_LIVE_CHAT_MODEL");
        assert!(client.models().await.unwrap().contains(&chat_model));

        let reply = client
            .chat(
                &chat_model,
                &[
                    ChatMessage {
                        role: Role::System,
                        content: "Answer with one word only.".to_string(),
                    },
                    user("Vad heter huvudstaden i Sverige?"),
                ],
                Some(200),
            )
            .await
            .unwrap();
        assert!(reply.to_lowercase().contains("stockholm"), "{reply:?}");

        if let Ok(embedding_model) = std::env::var("LAX_LIVE_EMBEDDING_MODEL") {
            let inputs = ["Offert för Q3".to_string(), "Q3 proposal".to_string()];
            let vectors = client.embeddings(&embedding_model, &inputs).await.unwrap();
            assert_eq!(vectors.len(), 2);
            assert!(vectors[0].len() > 100 && vectors[0].len() == vectors[1].len());
        }
    }

    /// One server-sent event carrying `content` as the next piece.
    fn piece(content: &str) -> String {
        let event = serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]
        });
        format!("data: {event}\n\n")
    }

    fn finished(reason: &str) -> String {
        format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"{reason}\"}}]}}\n\n"
        )
    }

    async fn stream(client: &ModelClient) -> (Result<String, ModelError>, Vec<String>) {
        let mut pieces = Vec::new();
        let reply = client
            .chat_stream("gemma", &[user("Say hello")], Some(20), |p| {
                pieces.push(p.to_string())
            })
            .await;
        (reply, pieces)
    }

    /// A stream of `parts`, each written on its own.
    fn events(parts: Vec<String>) -> Reply {
        Reply::Stream {
            parts: parts.into_iter().map(String::into_bytes).collect(),
            hold: false,
        }
    }

    #[tokio::test]
    async fn a_streamed_reply_comes_in_pieces() {
        let role = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n";
        let body = [
            role.to_string(),
            piece(r#"{"answer": "Hej"#),
            piece(" Åsa"),
            piece(r#"", "sources": [1]}"#),
            finished("stop"),
            "data: [DONE]\n\n".to_string(),
        ]
        .concat()
        .into_bytes();
        // Written in three parts: one cuts an event in two, the other cuts
        // "Å", which takes two bytes.
        let inside = body.windows(2).position(|w| w == "Å".as_bytes()).unwrap() + 1;
        let parts = vec![
            body[..20].to_vec(),
            body[20..inside].to_vec(),
            body[inside..].to_vec(),
        ];
        let server = MockServer::start_with(vec![Reply::Stream { parts, hold: false }]);
        let client = server.client().without_reasoning();
        let (reply, pieces) = stream(&client).await;
        assert_eq!(reply.unwrap(), r#"{"answer": "Hej Åsa", "sources": [1]}"#);
        assert_eq!(
            pieces,
            [r#"{"answer": "Hej"#, " Åsa", r#"", "sources": [1]}"#]
        );
        let sent: serde_json::Value = serde_json::from_str(&server.requests()[0].1).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["reasoning_effort"], "none");
    }

    #[tokio::test]
    async fn a_stream_without_done_ends_with_the_body() {
        let parts = vec![piece("Hej"), piece(" då")];
        let server = MockServer::start_with(vec![events(parts)]);
        let (reply, _) = stream(&server.client()).await;
        assert_eq!(reply.unwrap(), "Hej då");
    }

    #[tokio::test]
    async fn a_reasoning_model_s_thinking_is_never_passed_on() {
        let thinking = "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Anna Example is\"}}]}\n\n";
        let parts = vec![
            thinking.to_string(),
            piece("Hej"),
            "data: [DONE]\n\n".to_string(),
        ];
        let server = MockServer::start_with(vec![events(parts)]);
        let (reply, pieces) = stream(&server.client()).await;
        assert_eq!(reply.unwrap(), "Hej");
        assert_eq!(pieces, ["Hej"]);
    }

    #[tokio::test]
    async fn a_stream_that_runs_out_of_room_is_told_apart() {
        let parts = vec![finished("length"), "data: [DONE]\n\n".to_string()];
        let server = MockServer::start_with(vec![events(parts)]);
        assert_eq!(stream(&server.client()).await.0, Err(ModelError::OutOfRoom));
    }

    #[tokio::test]
    async fn errors_in_a_stream_never_carry_what_the_server_sent() {
        for bad in [
            "data: {\"error\":{\"message\":\"Anna Example's diagnosis\"}}\n\n",
            "data: Anna Example's diagnosis\n\n",
        ] {
            let parts = vec![piece("Hej"), bad.to_string()];
            let server = MockServer::start_with(vec![events(parts)]);
            let error = stream(&server.client()).await.0.unwrap_err();
            assert_eq!(error, ModelError::InvalidResponse);
            assert!(!error.to_string().contains("Anna"));
        }
    }

    #[tokio::test]
    async fn a_server_that_does_not_stream_is_read_whole() {
        let server = MockServer::start(vec![(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"Hej Anna"}}]}"#,
        )]);
        let (reply, pieces) = stream(&server.client()).await;
        assert_eq!(reply.unwrap(), "Hej Anna");
        assert_eq!(pieces, ["Hej Anna"]);
        // Also when it says it is something else.
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"Hej Bo"}}]}"#;
        let server = MockServer::start_with(vec![events(vec![body.to_string()])]);
        let (reply, pieces) = stream(&server.client()).await;
        assert_eq!(reply.unwrap(), "Hej Bo");
        assert_eq!(pieces, ["Hej Bo"]);
    }

    #[tokio::test]
    async fn keep_alives_in_a_stream_are_passed_over() {
        let parts = vec![
            ": ping\n\n".to_string(),
            "data:\n\n".to_string(),
            "data: \n\n".to_string(),
            piece("Hej"),
            "data: [DONE]\n\n".to_string(),
        ];
        let server = MockServer::start_with(vec![events(parts)]);
        let (reply, pieces) = stream(&server.client()).await;
        assert_eq!(reply.unwrap(), "Hej");
        assert_eq!(pieces, ["Hej"]);
    }

    #[tokio::test]
    async fn a_stream_is_asked_again_without_no_reasoning_if_refused() {
        let server = MockServer::start_with(vec![
            Reply::Json(400, r#"{"error":"unknown reasoning_effort"}"#),
            events(vec![piece("Hej")]),
        ]);
        let (reply, _) = stream(&server.client().without_reasoning()).await;
        assert_eq!(reply.unwrap(), "Hej");
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let second: serde_json::Value = serde_json::from_str(&requests[1].1).unwrap();
        assert!(second.get("reasoning_effort").is_none());
        assert_eq!(second["stream"], true);
    }

    #[tokio::test]
    async fn a_pause_stops_a_stream_within_a_second() {
        use crate::pause::{Pause, PauseLength};
        let server = MockServer::start_with(vec![Reply::Stream {
            parts: vec![piece("Hej").into_bytes()],
            hold: true,
        }]);
        let client = server.client();
        let pause = Pause::default();
        let mut pieces = Vec::new();
        let messages = [user("x")];
        let streaming = client.chat_stream("gemma", &messages, None, |p| {
            pieces.push(p.to_string());
        });
        let pause_soon = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            pause.pause(PauseLength::UntilResumed);
            Instant::now()
        };
        let (reply, paused_at) = tokio::join!(pause.unless_paused(streaming), pause_soon);
        assert_eq!(reply, None);
        assert!(paused_at.elapsed() < Duration::from_millis(1500));
        assert_eq!(pieces, ["Hej"]);
        // The connection is closed, so the server can stop writing.
        let waiting = Instant::now();
        while !server.closed.load(Ordering::SeqCst) {
            assert!(waiting.elapsed() < Duration::from_secs(2), "still open");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn remote_servers_are_refused_for_now() {
        let remote = Endpoint::parse("https://api.example.com/v1").unwrap();
        assert!(matches!(
            ModelClient::new(remote),
            Err(ModelError::RemoteNotAllowed)
        ));
    }
}
