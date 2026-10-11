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
        #[derive(Serialize)]
        struct Request<'a> {
            model: &'a str,
            messages: &'a [ChatMessage],
            #[serde(skip_serializing_if = "Option::is_none")]
            max_tokens: Option<u32>,
            stream: bool,
        }
        #[derive(Deserialize)]
        struct Reply {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: Message,
            finish_reason: Option<String>,
        }
        #[derive(Deserialize)]
        struct Message {
            content: Option<String>,
        }
        let request = Request {
            model,
            messages,
            max_tokens,
            stream: false,
        };
        let reply: Reply = self
            .call(
                "model.chat",
                "chat/completions",
                Some(&request),
                CHAT_TIMEOUT,
            )
            .await?;
        let choice = reply
            .choices
            .into_iter()
            .next()
            .ok_or(ModelError::InvalidResponse)?;
        match choice.message.content {
            Some(content) if !content.trim().is_empty() => Ok(content),
            _ if choice.finish_reason.as_deref() == Some("length") => Err(ModelError::OutOfRoom),
            _ => Err(ModelError::InvalidResponse),
        }
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
        let url = self.endpoint.url(path);
        let mut attempts = 0;
        let result = loop {
            attempts += 1;
            let request = match body {
                Some(body) => self.http.post(url.clone()).json(body),
                None => self.http.get(url.clone()),
            };
            // Only retry what didn't reach the server (a failed connection)
            // or what the server turned down without handling (busy, down).
            // Anything else may have been received, and sending it again
            // would send its content again.
            let (outcome, retry) = match request.timeout(timeout).send().await {
                Ok(response) => {
                    let outcome = read(response).await;
                    let busy = matches!(outcome, Err(ModelError::Status(429 | 502 | 503 | 504)));
                    (outcome, busy)
                }
                Err(error) if error.is_timeout() => (Err(ModelError::Timeout), false),
                Err(error) => (Err(ModelError::Unreachable), error.is_connect()),
            };
            match self.backoff.get(attempts - 1) {
                Some(wait) if retry && self.retries => tokio::time::sleep(*wait).await,
                _ => break outcome,
            }
        };
        let status = match result {
            Ok(_) => 200,
            Err(ModelError::Status(code)) => u64::from(code),
            Err(_) => 0,
        };
        logging::info(
            event,
            &[
                Field::Count("status", status),
                Field::Count("attempts", attempts as u64),
                Field::Duration("elapsed", started.elapsed()),
            ],
        );
        let bytes = result?;
        serde_json::from_slice(&bytes).map_err(|_| ModelError::InvalidResponse)
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

/// The body of a successful response, at most `MAX_RESPONSE_BYTES`.
async fn read(mut response: Response) -> Result<Vec<u8>, ModelError> {
    let status = response.status();
    if status != StatusCode::OK {
        return Err(ModelError::Status(status.as_u16()));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// A tiny HTTP server on 127.0.0.1 that answers with `replies`, one per
    /// request, and records each request's first line and body.
    struct MockServer {
        address: String,
        requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl MockServer {
        fn start(replies: Vec<(u16, &'static str)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = format!("http://{}/v1", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            thread::spawn(move || {
                for (status, body) in replies {
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
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(reply.as_bytes()).unwrap();
                }
            });
            MockServer { address, requests }
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
    /// LAX_LIVE_EMBEDDING_MODEL=text-embedding-nomic-embed-text-v1.5 \
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

    #[test]
    fn remote_servers_are_refused_for_now() {
        let remote = Endpoint::parse("https://api.example.com/v1").unwrap();
        assert!(matches!(
            ModelClient::new(remote),
            Err(ModelError::RemoteNotAllowed)
        ));
    }
}
