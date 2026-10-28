# MISAKA Studio

A local LLM desktop app: search Hugging Face, download a model, load it, chat with it — and serve
an OpenAI-compatible API on `localhost` while you do. Windows, macOS and Linux. Chat runs locally;
optional prompt mining sends work to the configured mining service.

What it has that other local-LLM apps do not: **every completion is recorded with the identity of
what produced it** — the model's `h_M`, the runtime's `h_R` and determinism class, and commitments
to the prompt and the answer, derived exactly the way MISAKA consensus derives them, in the
[misakas](https://github.com/MISAKA-BTC/misakas) node.

```
  Studio UI  (React — or any OpenAI client)
      │  HTTP:  /v1/…  and  /api/v1/…
  MISAKA Runtime API              misaka-studiod
      │  InferenceBackend
  LLM backend                     llama.cpp · MLX · (MISAKA's own runtime, later)
      │
  GPU / CPU
```

The UI and the runtime are separate processes that talk over that HTTP boundary and nothing else.
The desktop shell is a window around the same bundle the runtime serves. Which means: the API other
applications use is the API this app uses, and it cannot rot without the app breaking first.

## Status

**v0.1.2 — local desktop development build.** Model management, streaming chat, reasoning,
image generation, coding-agent sessions, workspaces, Git and terminals are available. A new chat
starts with a model selected; each chat retains its choice, including when loading fails, and
shows an actionable error. Reasoning has a bounded budget, and a reply that reaches its ceiling
without an answer is retried once with thinking disabled.

The Network tab uses the node-free testnet-12 paths described below. Earlier supervised-node and
hosted testnet-11 pool workflows have been removed. Public-network settlement depends on the
fleet being able to retrieve the attempt evidence; the dated integration findings in `docs/`
record the limitations observed during testing, rather than a guarantee of mining rewards.

## Quick start

```bash
# 1. The UI bundle, then the runtime — in that order: the runtime compiles the built
#    UI into itself, so a binary built before `npm run build` has no app inside it.
npm --prefix ui install && npm --prefix ui run build
cargo build --release                    # misaka-studiod

# 2. An engine. The Studio drives llama.cpp; it does not bundle it — it installs it.
#    Settings → Backend → "Install a llama.cpp build" downloads upstream's build for this
#    machine's GPU (Metal / CUDA / Vulkan / ROCm), verifies it, and points the setting at it.
#    Or install llama.cpp yourself so `llama-server` is on PATH, or name your own build.

# 3. Run — from any directory; the UI travels inside the binary
./target/release/misaka-studiod
#   → http://127.0.0.1:1338
```

`--ui-dir ui/dist` still works and takes precedence: it serves the directory live, so
`npm --prefix ui run build` alone refreshes the app with no Rust recompile. Without it the
embedded copy is served. `misaka-studiod --check` prints which of the two you are getting.

No engine installed and just want to see the app? `--backend mock` streams canned replies with no
model at all:

```bash
./target/release/misaka-studiod --backend mock
```

### On Windows

The block above is bash. **Windows PowerShell 5.1 — the one that ships with Windows — has no
`&&`**, and reports `&& はこのバージョンでは有効なステートメント区切りではありません`
("&& is not a valid statement separator in this version"). Use `;`, backslashes, and the `.exe`:

```powershell
cargo build --release
npm --prefix ui install; npm --prefix ui run build
.\target\release\misaka-studiod.exe
```

PowerShell 7 (`pwsh`) does support `&&`, so the bash lines work there unchanged apart from the
path separators. Git Bash and WSL run the bash block as written.

### The desktop app

```bash
cd desktop/src-tauri
cargo tauri dev          # or: cargo tauri build   (needs the Tauri CLI: cargo install tauri-cli)
```

The shell spawns `misaka-studiod` itself, waits for it to be healthy, and stops it on exit — unless
one was **already** running on the configured port, in which case it attaches to that one rather
than starting a rival that would load the same model into the same GPU twice.

Linux needs the webview development packages (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`); macOS and
Windows have their webviews already. Bundle icons for Windows and macOS are generated from
`desktop/src-tauri/icons/icon.png` with `cargo tauri icon`.

## Working in the window

The window has two sides, switched at the top of the sidebar the way Claude's desktop app does it:
**Chat** — conversations with the loaded model — and **Code** — coding-agent sessions, grouped by the
project folder they work in. Each side remembers the thread it had open. And it is built to be driven
from the keyboard, the way an editor is.

| Key | Does |
|---|---|
| `⌘⇧1` · `⌘⇧2` | Chat · Code |
| `⌘K` / `Ctrl+K` | Command palette: every action, every thread (titles and message text), every installed model, and the prompt-mining controls |
| `⌘N` (`⌘⇧O` in a browser tab) · `⌘⇧A` | New chat or session on this side · new Code session |
| `⌘B` · `⌘,` · `⌘1`…`⌘6` | Sidebar · Settings · the six views |
| `⌘L` · `⌘/` | Focus the composer · every shortcut |
| `⌘⌥B` · `⌘⇧G` · `⌘J` · `⌘⇧E` | Code: session summary · changes · terminal · files |
| `⌘⇧M` | Mining panel |
| `Esc` | Stop the reply or the agent |

**The Code side** is a session per task, in a project folder. Beside it: a **session summary** (the
folder, its git branch and how many files are changed, the approval policy, how full the model's
window is, what the session has changed and mined); **Changes** — the working tree's git status with
a diff for every file, and below it the agent's own edits with Revert; a **terminal** bound to the
folder (output streams as it is written, Ctrl+C stops the command and everything it started, and a
block's output can be sent to the agent); and a **file browser** with a highlighted viewer and
"Mention" to hand a file to the agent.

**The composer carries everything you change while working**: the model (with the context size to
load it at), attached text files (drag them in, paste them, or `@`-mention a file in a Code session),
and `/` commands — `/model`, `/clear`, `/rename`, `/export`, `/mine on|off`, `/think on|off`,
`/chat`, `/code`, `/help`. A thinking model's reasoning streams into a block above its
answer and folds to "Thought for 12s" when the answer starts. Threads are grouped by day in the
sidebar; star, rename (double-click) and export them as Markdown from there or from the palette.

**Code sessions work in a folder.** Open one (`⌘⇧A`, or Open folder… on the Code side), describe a task, and the
local model lists, reads and searches files, edits them, and runs commands in that folder — one
tool call per step, in the Qwen3 XML tool format that small local models follow most reliably. What
it may do without asking is the thread's choice: *Read only*, *Ask* (every edit and command waits),
*Auto edit* (the default — edits apply at once, commands wait), or *Full auto*. Every edit lands in
the **Changes** panel as a diff, and Revert puts the file back byte for byte (it refuses when the
file has changed since, unless told otherwise). Give the agent a window of at least 16K tokens —
32K is comfortable; the model picker loads at either. The mining gateway's 512-token class cannot
run an agent at all: see below for how agent work and mining go together.

The folder API behind this (`/api/v1/workspace/*`) reads and writes files and runs commands, so it
has the strictest of the API's guards: served only on a loopback bind, only to this machine, and
only to the Studio's own window — a page on any other origin, an origin listed in
`server.cors_origins`, or a page reached through DNS rebinding is refused before anything is read.

**Prompt mining is part of the window, not a separate tab.** The ⛏ chip in the composer says what
happens to what you type — mined *inline* (the gateway is the chat engine, so the answer is the
work), mined in the *background* (the chat answers locally, the prompt is queued for the slot), or
not mined, and why. The status bar keeps the queue's counts in view, the Mining panel lists what
this thread has had mined and the claims it produced, and mining can be turned off per thread.
Background mining is how an agent thread mines: the agent runs on the local engine and the task
you typed — only what you typed, never file contents or tool output — goes to the queue.

## Using it as an API

Point anything that speaks OpenAI at `http://127.0.0.1:1338/v1`:

```bash
curl http://127.0.0.1:1338/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"Qwen3-4B-Instruct-Q4_K_M","messages":[{"role":"user","content":"hello"}],"stream":true}'
```

`/v1/chat/completions`, `/v1/completions` and `/v1/models`, with SSE streaming and the same chunk
shape OpenAI sends. Naming a model that is not loaded loads it. `top_k`, `min_p` and
`repeat_penalty` are accepted as extra fields, because those are what local engines actually
expose.

The Studio's own API lives under `/api/v1` — models with fit verdicts, catalog search, downloads,
metrics, settings, provenance records. `misaka-studiod --help` lists the flags;
`misaka-studiod --check` prints where everything resolved to.

**The default bind is `127.0.0.1` and there is no authentication.** Binding anywhere else without
`--api-key` is refused at startup rather than served: an open inference endpoint on a shared
network is the kind of mistake that stays open for a week before anyone notices.

**Loopback is not the same as private, though**: every website open in a browser on this machine
can send requests to `127.0.0.1`. `/v1` stays open to them — calling the local model is what an
OpenAI-compatible endpoint is for — but the Studio's own API, `/api/v1` (settings, downloads,
engines, the node, the record log), answers only the Studio's window: requests with no `Origin`
(curl, scripts, the desktop shell), loopback origins such as the Vite dev server, and the Tauri
window. Anything else gets a 403, and so does a `Host` that is not this machine (DNS rebinding). To
let a web app you trust call it, list its origin in `server.cors_origins` in the settings file —
`["https://app.example"]`; `["*"]` restores "any origin" for `/api/v1`, deliberately. The folder API
under `/api/v1/workspace` is never opened by that list.

## What it does that is worth knowing about

**It tells you whether a model will run *before* you download it.** A size in bytes does not
answer that question. Every model is shown with its real memory bill — weights plus the KV cache at
the context it would actually load with, plus compute overhead — against what this machine has,
including Apple Silicon's unified memory (where "0 MB VRAM" is the most misleading thing an app can
say).

**Downloads resume and are verified.** Hugging Face publishes each LFS object's SHA-256; a finished
file is checked against *that*, not against itself. A partial download lives in a `.part` file and
never appears as a model until it is one.

**A conversation fits the model's window, and you can see how.** A class registered at 512
tokens holds a question and an answer, not a conversation. Each request carries, in order: the
system prompt and the question; the conversation's **pinned notes** (standing facts you pin from
the chat, kept ahead of all history); recent turns whole; and a memory of the older ones — an
extract of each earlier result, or a summary from a local GGUF model if one is chosen under
Settings → Context. Prompts are counted with the class's own tokenizer (fetched beside its
artifact and verified against a pinned digest) rather than estimated. Under every reply that was
fitted, one line says what the model was given, and a click shows the messages exactly as sent.

**The engine is a child process, not a library.** A model that crashes llama.cpp on a driver fault
takes the engine with it, not the app — and the engine can be updated without rebuilding the
Studio.

**GPU offload is planned, not guessed — and then checked.** "Auto" asks the engine which devices it
can drive (`llama-server --list-devices`), computes how many layers fit on that device after the KV
cache and scratch buffers are accounted for, and reads the engine's own load log for where the
weights actually went. The model bar shows the result — `29/29 on MTL0` — or `CPU`, with the reason
beside it. A `llama-server` from a package repository is usually a CPU-only build that takes
`--n-gpu-layers 99` in silence; the Studio now says so, and Settings → Backend installs the build
that would use the card.

## Joining the MISAKA network

The Studio defaults to testnet-12 and reads public nodes over wRPC. It does not run `kaspad` or
join a hosted pool. The Network tab exposes two local execution paths:

* **Remote mining:** supervises `palw-remote-miner` and `palw-attempt-exec`, with the configured
  bond key, checkpoint, class and public endpoints. The miner performs and signs work locally;
  public nodes validate submitted blocks. Its verification labels and checkpoint requirements
  remain visible before mining starts.
* **Prompt mining:** supervises the local gateway and submitter. Background mode queues prompts
  while a separate local engine answers chat. The queue reports why it is unavailable when the
  gateway is stopped or no local chat engine can serve the selected model.

Models → Discover lists the selected network's execution classes. testnet-12's genesis table
contains the deterministic floor, `PALW-QWEN25-A16-8K` and `PALW-QWEN25-A16-2M`. Downloads are
size- and SHA-256-pinned, and readiness includes the replay/attempt memory requirement. Published
PALWART3 models that are not registered classes are listed separately: installing one does not
make it eligible to mine on the network.

A submitted attempt's evidence must remain available to the Panel until settlement. See the
[October 25 integration report](docs/feedback-t12-nodeless-mining-2026-10-25.md) and
[October 26 evidence report](docs/feedback-t12-nodeless-evidence-2026-10-26.md) for the tested fleet
revisions, dependencies and outstanding deployment requirements.

## Provenance — the part that is MISAKA's

The long path is `Inference → Deterministic Execution → Inference Hash → Verification → Compute
Credit → PALW → MISAKA Network`. This version implements the first and third links and nothing
beyond them. There is no chain client here, no bond, no credit.

That is the design decision, not a shortcut. Verification can be added on top of a record that
exists; a record cannot be reconstructed after the fact. An app that does not write down which
artifact answered a prompt has destroyed the evidence.

So each completion produces an [`InferenceRecord`](crates/misaka-studio-core/src/provenance.rs):

| field | what it is |
|---|---|
| `h_M` | model identity — keyed BLAKE2b-512 over the GGUF's SHA-256, size, filename, repo and revision |
| `h_R` | runtime identity — engine commit, patch digest, build number, build profile |
| `class_id` | determinism class — the set of runtimes expected to agree bit for bit |
| `prompt_commitment` / `output_commitment` | commitments to the bytes, under Studio-local domains |
| `inference_hash` | what the whole record reduces to |
| `replayability` | `deterministic`, `seeded_sampling` or `unrepeatable` — stated, never implied |

The first three are **the consensus derivations** — same keyed BLAKE2b-512, same domain keys, same
field order as [`consensus/core/src/vlt.rs`](https://github.com/MISAKA-BTC/misakas/blob/main/consensus/core/src/vlt.rs) in the
[misakas](https://github.com/MISAKA-BTC/misakas) node, cross-checked in tests against vectors produced by a third
implementation. A model downloaded here already carries the `h_M` a validator would compute for
it.

Timing is recorded and deliberately **not** committed: a verifier re-running the job on other
hardware must reach the same `inference_hash`.

Records are JSONL at `<data dir>/inference-records.jsonl` — readable with `tail -f` and `jq`,
auditable without this app. Prompt and completion **text** is not stored unless you turn on
`provenance.keep_transcripts`: the record commits to the bytes with a hash, and a provenance log
that quietly duplicates every conversation is a second copy of your data in a place you did not
choose.

## Layout

```
crates/misaka-studio-core/      GGUF reader, quantization table, hardware probe, fit
                                arithmetic, settings, provenance derivations, the PALW
                                mining-class catalog
crates/misaka-studio-runtime/   backends, model store, Hugging Face catalog, downloads,
                                metrics, record log, node supervisor + wRPC client,
                                the HTTP API, `misaka-studiod`
ui/                             React + Vite + Tailwind; one bundle, served or loaded from disk
desktop/src-tauri/              the Tauri v2 shell
testing/                        make_tiny_gguf.py — a real GGUF small enough to test with
docs/                           the design document
NOTICE                          attribution — Jan (Apache-2.0), llama.cpp, MLX
```

## Adding a backend

Implement [`InferenceBackend`](crates/misaka-studio-runtime/src/backend/mod.rs) — `load`, `unload`,
`generate`, `descriptor`, `availability` — and add an arm to `build_backend`. Nothing above that
trait names an engine. If the engine already speaks OpenAI over HTTP, `ChildEngine` supplies the
process supervision, health wait and SSE parsing, and the backend is about eighty lines (see
`llamacpp.rs`).

`descriptor()` is the part to get right: it becomes `h_R`, and a backend that cannot prove its
build flags must record the literal `unknown` rather than a plausible string. An identity derived
from a guess is worse than none.

## Tests

```bash
cd misaka-studio && cargo test           # runtime and core
npm --prefix ui run build                # typecheck + bundle
npm --prefix ui test                     # the UI's logic: history, the agent's protocol and loop, diffs, fuzzy search…
```

The mock backend exists so the streaming, metrics and provenance paths are testable with no GPU and
no multi-gigabyte download — which is what makes them testable in CI at all.

### Against a real engine

The mock cannot tell you whether the llama.cpp backend actually starts a process, survives the
health wait, parses that engine's SSE framing, or reads a version banner a real binary printed. So
there is a test that does, and a fixture small enough to make it cheap:

```bash
# A ~50 kB llama-architecture GGUF with random weights. It generates nonsense; nothing here tests
# what a model says.
python3 testing/make_tiny_gguf.py /tmp/misaka-models/tiny-llama-F32.gguf

MISAKA_TEST_LLAMA_SERVER=/path/to/llama-server \
MISAKA_TEST_MODELS_DIR=/tmp/misaka-models \
  cargo test -p misaka-studio-runtime --test llamacpp_e2e -- --nocapture
```

It skips itself, loudly, when those are not set. Three bugs were found by running it and by no
other means: llama.cpp's current version banner (`version: 0.3.0-dev (build 1, commit 90c26fc)`)
parsed to a commit that was a whole sentence; `-fa` as a bare flag is now an error, so every load
died in a usage message; and a model with no chat template answered `400 unordered_map::at`.

## Licensing

MISAKA Studio is ISC, the same licence as the MISAKA node ([misakas](https://github.com/MISAKA-BTC/misakas)). Jan
(Apache-2.0) is the reference implementation this was designed against; llama.cpp (MIT) and MLX
(MIT) are driven as external processes and not bundled. No closed-source application was used,
copied or reverse-engineered. See [NOTICE](NOTICE) for the details and for what must happen if Jan
source is ever vendored here.
