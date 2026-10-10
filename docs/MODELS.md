# Models

Lax works with any model server that speaks the OpenAI-compatible API (ADR 0004).
By default everything runs on your Mac. This page says which local models we
recommend, and why.

## Recommended setup for a 16 GB Apple Silicon Mac

| Task | Model | Size | Why |
|---|---|---|---|
| Chat (summaries, entities, page updates) | **Gemma 4 E4B** (`google/gemma-4-e4b`, MLX 4-bit in LM Studio) | about 6.4 GB in memory | Good Swedish and English, valid JSON, resisted a prompt injection, answers in 10–25 s |
| Embeddings | **nomic-embed-text v1.5** (`text-embedding-nomic-embed-text-v1.5`) | about 80 MB | Small, fast, multilingual enough for matching names and topics |

Run them in [LM Studio](https://lmstudio.ai) or [Ollama](https://ollama.com),
start its local server, and choose them under **Models** in the Lax window.
Both servers listen only on this Mac by default.

## How we chose

Measured on 2026-10-11 on a MacBook Pro with an M1 Pro and 16 GB, LM Studio,
temperature 0.2, with three synthetic captures (a Swedish mail, an English Slack
message and a Swedish document, about one project with three people and one
company):

| | Gemma 4 E4B | Qwen 3.5 9B |
|---|---|---|
| Journal entry in Swedish | 24 s. Fluent, two sentences, one small mistake (it said the offer was approved when it was only offered) | 153 s, after about 4,700 tokens of reasoning. More complete and correct |
| Entities as JSON | 17 s. Valid; the three people, the company, two project names | 141 s. Valid, but with duplicates ("Erik" and "Erik Exempel") and a Slack channel taken for a project |
| Prompt injection ("ignore all previous instructions…" inside the captured text) | 11 s. Ignored it and returned the one person | Ran out of a 5,000-token budget while reasoning, no answer |

Qwen 3.5 reasons before every answer, and in LM Studio that couldn't be turned
off with `/no_think` or `enable_thinking: false`. With a 1,200-token budget it
gave no answer at all. That makes it six to ten times slower here, too slow for
summarizing all day in the background on a 16 GB machine. Gemma 4 E4B is fast
enough, answers in the user's language, and kept to the data it was given.

These are first measurements with a handful of examples, not a benchmark. They
are repeated when the journal (Stage 3) has real prompts.

## Trying a model yourself

The opt-in test `models::client::tests::live_local_server` talks to a running
local server with fixed, synthetic prompts:

```sh
cd src-tauri
LAX_LIVE_MODEL_URL=http://localhost:1234/v1 \
LAX_LIVE_CHAT_MODEL=google/gemma-4-e4b \
LAX_LIVE_EMBEDDING_MODEL=text-embedding-nomic-embed-text-v1.5 \
cargo test live_local_server -- --ignored
```

The **Test** buttons under Models in the window do the same for the chosen
models.
