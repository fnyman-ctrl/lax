# Models

Lax works with any model server that speaks the OpenAI-compatible API (ADR 0004).
By default everything runs on your Mac. This page says which local models we
recommend, and why.

## Recommended setup for a 16 GB Apple Silicon Mac

| Task | Model | Size | Why |
|---|---|---|---|
| Chat (summaries, entities, page updates) | **Gemma 4 E4B** (`google/gemma-4-e4b`, MLX 4-bit in LM Studio) | about 6.4 GB in memory | Good Swedish and English, valid JSON, resisted a prompt injection, answers in 10–25 s |
| Embeddings | **bge-m3** (`text-embedding-bge-m3`, GGUF Q8_0 in LM Studio; `bge-m3` in Ollama) | about 0.6 GB | Multilingual, good in Swedish and English and across them, needs no prefixes; see the comparison below |

Run them in [LM Studio](https://lmstudio.ai) or [Ollama](https://ollama.com),
start its local server, and choose them under **Models** in the Lax window.
Both servers listen only on this Mac by default.

### Keep both models loaded

Lax uses the embedding model and the chat model one right after the other:
every question in Ask, and the memory's work in the background. LM Studio
keeps only the last model it loaded on demand ("Only Keep Last JIT Loaded
Model", on by default), so it unloads the chat model for each search and
loads all 6 GB of it again for the answer. Turn that setting off under
**Developer → Server Settings**, or load both models yourself with
`lms load`.

Measured on 2026-10-13 on the same M1 Pro, with nomic-embed-text v1.5 as the
embedding model. bge-m3 takes about 6 s to load instead of under 1 s, so with
the setting on, a search is slower still:

| | Setting on | Both models loaded |
|---|---|---|
| A search (embedding) | 1.5–3.7 s | under 0.1 s |
| A short answer right after a search | 13.4 s | 0.4–0.6 s |
| An answer from about 1,900 tokens of context | 20.7 s | about 3 s |

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

Measured again on 2026-10-19 on a MacBook Pro with an M1 Max and 64 GB, in LM
Studio 0.4.26: `reasoning_effort: "none"`, which Lax sends for Ask and the
writing assistant, does turn Qwen 3.5 9B's reasoning off. With reasoning on,
it spent the whole 2,000-token budget of a journal entry on reasoning, even
for two short synthetic captures, and gave no answer after 56 s. Asked not to
reason, it wrote the entry in 1.9 s. The journal and the memory let models
reason, so when a model runs out of room, Lax asks it once more not to
reason. With Qwen 3.5 the journal works that way, but each entry first spends
about a minute reasoning.

These are first measurements with a handful of examples, not a benchmark. They
are repeated when the journal (Stage 3) has real prompts.

### The embedding model

Measured on 2026-10-15 on the same Mac, in LM Studio, with 50 synthetic journal
entries in Lax's style (mostly Swedish, some English, several about the same
client or topic) and 50 questions with known answers. The 20 hard questions
share no keywords with their entry, ask in the other language, or have a near
miss next to the answer. "Top 3" counts how often the right entry was among
the first three, which matters most, since Ask gives the chat model several.
Texts were sent as Lax sends them, without prefixes.

| | Direct questions, top 3 | Hard questions, top 3 | Hard questions, first | Size |
|---|---|---|---|---|
| nomic-embed-text v1.5 (recommended until now) | 60% | 40% | 25% | 84 MB |
| **bge-m3** (Q8_0) | **100%** (100% first) | **90%** | 45% | 635 MB |
| nomic-embed-text v2-moe (Q4_K_M) | 100% | 90% | 55% | 344 MB |
| Qwen3-Embedding 0.6B (Q8_0) | 97% | 85% | 45% | 639 MB |

nomic v1.5 is trained on English and misses most Swedish paraphrases. bge-m3
and nomic v2-moe are about equally good; the difference on the hard questions
is two or three questions out of 20. bge-m3 is recommended because it needs no
query or document prefixes, which Lax doesn't send, reads up to 8,192 tokens
(v2-moe 512), and is MIT licensed. Every model embeds a question in 10–40 ms,
so speed doesn't decide it.

To get it in LM Studio: `lms get https://huggingface.co/gpustack/bge-m3-GGUF@q8_0`,
or search for bge-m3 in the app. When you switch embedding models under
**Models**, Lax gives every journal entry a new vector in the background, 16 at
a time while the Mac is idle and on power. Until it is done, Ask finds fewer
entries by meaning.

## Trying a model yourself

The opt-in test `models::client::tests::live_local_server` talks to a running
local server with fixed, synthetic prompts:

```sh
cd src-tauri
LAX_LIVE_MODEL_URL=http://localhost:1234/v1 \
LAX_LIVE_CHAT_MODEL=google/gemma-4-e4b \
LAX_LIVE_EMBEDDING_MODEL=text-embedding-bge-m3 \
cargo test live_local_server -- --ignored
```

The **Test** buttons under Models in the window do the same for the chosen
models.
