//! Vectors of journal episodes, for finding them by meaning when the user
//! asks (Stage 5).
//!
//! - Only with an embedding model chosen, and only models on this Mac
//!   (GUARDRAILS §3). Runs in the journal's loop, in turn at the model, a
//!   batch at a time, and not while paused.
//! - A vector is derived from an episode, and deleted with it.
//! - Logs hold counts and timings only.

use std::sync::{Mutex, PoisonError};

use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::models::{Endpoint, ModelClient, ModelError, ModelTurn};
use crate::pause::Pause;
use crate::store::{ModelTask, Store};

/// Episodes given vectors per round.
const BATCH: usize = 16;

/// One round. `Err` means the model server is unavailable.
pub async fn round(app: &AppHandle) -> Result<(), ()> {
    if app.state::<Pause>().is_paused() {
        return Ok(());
    }
    let work = with_store(app, |store| -> crate::store::Result<_> {
        let Some(choice) = store.model_choice(ModelTask::Embeddings)? else {
            return Ok(None);
        };
        let episodes = store.unvectored_episodes(&choice.model, BATCH)?;
        Ok((!episodes.is_empty()).then_some((choice, episodes)))
    })
    .and_then(Result::ok)
    .flatten();
    let Some((choice, episodes)) = work else {
        return Ok(());
    };
    let started = std::time::Instant::now();
    let texts: Vec<String> = episodes
        .iter()
        .map(|(_, summary)| summary.clone())
        .collect();
    let turn = app.state::<ModelTurn>();
    let pause = app.state::<Pause>();
    let embedded = embed(&choice.base_url, &choice.model, &texts);
    let Some(result) = turn.background(&pause, embedded).await else {
        return Ok(());
    };
    let vectors: Vec<(i64, Vec<f32>)> = match result {
        Ok(vectors) if vectors.len() == episodes.len() => {
            episodes.iter().map(|(id, _)| *id).zip(vectors).collect()
        }
        Err(error) if error.unavailable() => {
            logging::warn(
                "vectors.failed",
                &[
                    Field::Flag("model_unavailable", true),
                    Field::Error(ErrorKind::Other),
                ],
            );
            return Err(());
        }
        // The batch failed for something in it: each episode on its own,
        // and one the model can't take gets an empty vector, which matches
        // nothing, so it isn't sent again every round.
        _ => {
            let mut one_by_one = Vec::new();
            for (id, summary) in &episodes {
                if app.state::<Pause>().is_paused() {
                    return Ok(());
                }
                let single = embed(
                    &choice.base_url,
                    &choice.model,
                    std::slice::from_ref(summary),
                );
                match turn.background(&pause, single).await {
                    None => return Ok(()),
                    Some(Err(error)) if error.unavailable() => return Err(()),
                    Some(Ok(mut v)) if v.len() == 1 => one_by_one.push((*id, v.remove(0))),
                    Some(_) => one_by_one.push((*id, Vec::new())),
                }
            }
            logging::warn(
                "vectors.failed",
                &[
                    Field::Flag("model_unavailable", false),
                    Field::Count(
                        "skipped",
                        one_by_one.iter().filter(|(_, v)| v.is_empty()).count() as u64,
                    ),
                ],
            );
            one_by_one
        }
    };
    let stored = with_store(app, |store| store.set_vectors(&choice.model, &vectors));
    logging::info(
        "vectors.batch",
        &[
            Field::Count("stored", stored.and_then(Result::ok).unwrap_or(0) as u64),
            Field::Duration("elapsed", started.elapsed()),
        ],
    );
    Ok(())
}

/// Vectors for `texts`, in order, from the embedding model.
pub async fn embed(
    base_url: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, ModelError> {
    let endpoint = Endpoint::parse(base_url).map_err(|_| ModelError::Unreachable)?;
    let client = ModelClient::new(endpoint)?;
    client.embeddings(model, texts).await
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&mut Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&mut store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::cosine;

    /// Against a real embedding model: `LAX_LIVE_MODEL_URL`,
    /// `LAX_LIVE_EMBEDDING_MODEL` (e.g. text-embedding-nomic-embed-text-v1.5).
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_EMBEDDING_MODEL"]
    async fn live_vectors_find_by_meaning() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let texts: Vec<String> = [
            "Skickade offerten för Q3 till Exempelbolaget och diskuterade priset.",
            "Felsökte inloggningen i appen och skrev tester.",
            "Läste nyheter om vädret.",
        ]
        .iter()
        .map(|t| t.to_string())
        .collect();
        let url = env("LAX_LIVE_MODEL_URL");
        let model = env("LAX_LIVE_EMBEDDING_MODEL");
        let vectors = embed(&url, &model, &texts).await.expect("vectors");
        let question = embed(
            &url,
            &model,
            &["Hur gick det med budgeten till kunden?".to_string()],
        )
        .await
        .expect("a vector");
        let best = (0..3)
            .max_by(|a, b| {
                cosine(&question[0], &vectors[*a]).total_cmp(&cosine(&question[0], &vectors[*b]))
            })
            .unwrap();
        assert_eq!(best, 0);
    }
}
