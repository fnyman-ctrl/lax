//! Choosing models in the window (#15): find model servers running on this
//! Mac, pick a chat and an embedding model, and try them.
//!
//! Only servers on this Mac can be chosen for now (GUARDRAILS §3); remote
//! providers come with the consent screen (#16). Looking for servers only
//! knocks on loopback ports, and only when the user asks.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::models::{ChatMessage, Endpoint, Locality, ModelClient, Role};
use crate::store::{ModelChoice, ModelTask, Store};

/// Model servers people run on a Mac, at their default addresses.
const KNOWN_SERVERS: &[(&str, &str)] = &[
    ("Ollama", "http://localhost:11434/v1"),
    ("LM Studio", "http://localhost:1234/v1"),
    ("llama.cpp", "http://localhost:8080/v1"),
    ("Jan", "http://localhost:1337/v1"),
];

const MAX_MODEL_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Task {
    Chat,
    Embeddings,
}

impl From<Task> for ModelTask {
    fn from(task: Task) -> Self {
        match task {
            Task::Chat => ModelTask::Chat,
            Task::Embeddings => ModelTask::Embeddings,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalServer {
    pub name: &'static str,
    pub base_url: String,
    pub models: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    pub chat: Option<Choice>,
    pub embeddings: Option<Choice>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub elapsed_ms: u64,
    /// What was checked, in a sentence; never the model's own words.
    pub detail: String,
}

/// How long a probed server gets to list its models.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The servers among `KNOWN_SERVERS` that answer, with their models. All
/// are asked at once, each for at most `PROBE_TIMEOUT`.
#[tauri::command]
pub async fn find_local_models() -> Vec<LocalServer> {
    let probes: Vec<_> = KNOWN_SERVERS
        .iter()
        .map(|(name, address)| tauri::async_runtime::spawn(probe(name, address)))
        .collect();
    let mut found = Vec::new();
    for probe in probes {
        if let Ok(Some(server)) = probe.await {
            found.push(server);
        }
    }
    found
}

async fn probe(name: &'static str, address: &'static str) -> Option<LocalServer> {
    let client = ModelClient::new(Endpoint::parse(address).ok()?).ok()?;
    let models = tokio::time::timeout(PROBE_TIMEOUT, client.without_retries().models())
        .await
        .ok()?
        .ok()?;
    Some(LocalServer {
        name,
        base_url: address.to_string(),
        models,
    })
}

#[tauri::command(async)]
pub fn model_settings(app: AppHandle) -> Result<ModelSettings, String> {
    with_store(&app, read_settings)
}

/// Chooses `model` on the server at `base_url` for `task`.
#[tauri::command(async)]
pub fn set_model(
    app: AppHandle,
    task: Task,
    base_url: String,
    model: String,
) -> Result<ModelSettings, String> {
    let choice = validate(&base_url, &model)?;
    with_store(&app, |store| {
        store
            .set_model_choice(task.into(), Some(&choice))
            .map_err(|_| "Lax couldn't save the choice.".to_string())?;
        read_settings(store)
    })
}

#[tauri::command(async)]
pub fn clear_model(app: AppHandle, task: Task) -> Result<ModelSettings, String> {
    with_store(&app, |store| {
        store
            .set_model_choice(task.into(), None)
            .map_err(|_| "Lax couldn't save the choice.".to_string())?;
        read_settings(store)
    })
}

/// Tries the chosen model with a fixed, synthetic request. Nothing captured
/// is sent.
#[tauri::command]
pub async fn test_model(app: AppHandle, task: Task) -> Result<TestResult, String> {
    let choice = with_store(&app, |store| {
        store
            .model_choice(task.into())
            .map_err(|_| "Lax couldn't read the choice.".to_string())
    })?
    .ok_or("Choose a model first.")?;
    let endpoint = Endpoint::parse(&choice.base_url).map_err(|e| e.to_string())?;
    let client = ModelClient::new(endpoint).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let (ok, detail) = match task {
        Task::Chat => {
            let reply = client
                .chat(
                    &choice.model,
                    &[
                        ChatMessage {
                            role: Role::System,
                            content: "Answer with one word only.".to_string(),
                        },
                        ChatMessage {
                            role: Role::User,
                            content: "Vad heter huvudstaden i Sverige?".to_string(),
                        },
                    ],
                    Some(400),
                )
                .await
                .map_err(|e| e.to_string())?;
            if reply.to_lowercase().contains("stockholm") {
                (
                    true,
                    "Asked in Swedish for the capital of Sweden, and it answered right."
                        .to_string(),
                )
            } else {
                (
                    false,
                    "It answered, but not with Stockholm when asked for the capital of Sweden."
                        .to_string(),
                )
            }
        }
        Task::Embeddings => {
            let vectors = client
                .embeddings(&choice.model, &["Offert för Q3".to_string()])
                .await
                .map_err(|e| e.to_string())?;
            let size = vectors.first().map_or(0, Vec::len);
            (
                size > 0,
                format!("Turned a test sentence into {size} numbers."),
            )
        }
    };
    Ok(TestResult {
        ok,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        detail,
    })
}

fn validate(base_url: &str, model: &str) -> Result<ModelChoice, String> {
    let endpoint = Endpoint::parse(base_url).map_err(|e| e.to_string())?;
    if endpoint.locality() != Locality::Local {
        return Err("Only model servers on this Mac can be chosen for now.".to_string());
    }
    let model = model.trim();
    if model.is_empty() || model.chars().count() > MAX_MODEL_CHARS {
        return Err("Choose a model.".to_string());
    }
    Ok(ModelChoice {
        base_url: endpoint.as_str().to_string(),
        model: model.to_string(),
    })
}

fn read_settings(store: &Store) -> Result<ModelSettings, String> {
    let read = |task| {
        store
            .model_choice(task)
            .map(|choice| {
                choice.map(|c| Choice {
                    base_url: c.base_url,
                    model: c.model,
                })
            })
            .map_err(|_| "Lax couldn't read the model settings.".to_string())
    };
    Ok(ModelSettings {
        chat: read(ModelTask::Chat)?,
        embeddings: read(ModelTask::Embeddings)?,
    })
}

fn with_store<T>(
    app: &AppHandle,
    work: impl FnOnce(&Store) -> Result<T, String>,
) -> Result<T, String> {
    let store = app
        .try_state::<Mutex<Store>>()
        .ok_or("The database isn't available.")?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    work(&store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_servers_can_be_chosen() {
        assert!(validate("http://localhost:1234/v1", "google/gemma-4-e4b").is_ok());
        assert!(validate("https://api.example.com/v1", "gpt").is_err());
        assert!(validate("http://192.168.1.10:11434/v1", "llama").is_err());
        assert!(validate("http://localhost:1234/v1", "  ").is_err());
        assert!(validate("http://localhost:1234/v1", &"m".repeat(201)).is_err());
    }

    #[test]
    fn choices_are_stored_with_a_normalised_address() {
        let choice = validate(" http://localhost:1234/v1 ", " gemma ").unwrap();
        assert_eq!(choice.base_url, "http://localhost:1234/v1/");
        assert_eq!(choice.model, "gemma");
    }

    #[test]
    fn known_servers_are_all_on_this_mac() {
        for (name, address) in KNOWN_SERVERS {
            let endpoint = Endpoint::parse(address).unwrap();
            assert_eq!(endpoint.locality(), Locality::Local, "{name}");
        }
    }

    /// Looks for real servers on this Mac. Not run by default; run with
    /// `LAX_LIVE_EXPECT_SERVER="LM Studio" cargo test live_find -- --ignored`.
    #[tokio::test]
    #[ignore = "needs a model server running on this Mac"]
    async fn live_find_local_models() {
        let expected = std::env::var("LAX_LIVE_EXPECT_SERVER").unwrap();
        let servers = find_local_models().await;
        let server = servers.iter().find(|s| s.name == expected).unwrap();
        assert!(!server.models.is_empty());
    }

    #[test]
    fn tasks_arrive_as_the_window_sends_them() {
        assert_eq!(
            serde_json::from_str::<Task>("\"chat\"").unwrap(),
            Task::Chat
        );
        assert_eq!(
            serde_json::from_str::<Task>("\"embeddings\"").unwrap(),
            Task::Embeddings
        );
    }
}
