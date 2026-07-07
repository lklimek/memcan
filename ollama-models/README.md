# Ollama model builds

Custom Ollama model builds maintained in this repo. Currently one:

## `gemma4-text:26b-a4b-it-qat` — projector-free build

A text-only build of Google's `gemma4:26b-a4b-it-qat` with the bundled
vision projector removed. Same text weights, byte-identical, same
Quantization-Aware Training (QAT) Q4_0 checkpoint — just without the
~1.2 GB multimodal component that vision-free deployments never use
(this build started life as MemCan's default model, but the fix applies
to any text-only deployment).

### Why this exists

Ollama loads and GPU-offloads a model's vision projector unconditionally
whenever one is present in the manifest, even if the client never sends an
image. On VRAM-constrained cards this has a real cost: Ollama's memory-fit
calculation reserves headroom for the projector even when it ends up on
CPU, which can push several of the *text* model's own MoE-expert layers off
GPU too — even though the text model alone would have fit entirely in VRAM.
See `docs/memcan-model-guide.html` in the [memcan repo](https://github.com/lklimek/memcan)
for the full VRAM picture; this is the fix for the specific "why isn't
this at 100% GPU" problem on borderline hardware (e.g. a 16 GB card).

Measured on one such card (RTX 5060 Ti, 16 GB): the stock multimodal model
landed at ~87% GPU residency (partial CPU offload); this build landed at
**100%** — and cold-load time dropped from 56–75s to ~7.6s.

### What's different from `gemma4:26b-a4b-it-qat`

Nothing about the text model itself. This is a manifest-level change, not
a re-quantization or re-training — the text weights are reused directly
from the same underlying blob. Only the projector layer (architecture
`clip`, type `gemma4v`) was dropped. See
`gemma4-text-26b-a4b-it-qat/NOTICE` for the formal Apache 2.0
modification notice and `gemma4-text-26b-a4b-it-qat/LICENSE` for the
full license text.

### Use it

```bash
ollama run lklimek/gemma4-text:26b-a4b-it-qat
```

No image support — that's the point. For multimodal use, pull the
original `gemma4:26b-a4b-it-qat` instead.

### Building it yourself

Requires only HTTP access to an Ollama instance that already has
`gemma4:26b-a4b-it-qat` pulled — no shell access to that host needed for
this step.

```bash
cd gemma4-text-26b-a4b-it-qat
uv run python build_text_only.py \
    --host http://<ollama-host>:11434 \
    --api-key "$OLLAMA_API_KEY" \
    --base gemma4:26b-a4b-it-qat \
    --new gemma4-text:26b-a4b-it-qat
```

The script fetches the manifest, extracts the text-model blob digest,
creates the new model, then loads it and reports the resulting GPU
residency percentage so you can confirm it actually landed at 100% on
your hardware before publishing anything.

**Note:** Ollama's `/api/create` re-validates the referenced blob during
its "verifying conversion" step, which needs roughly the model's own size
again in free disk space on the Ollama host (~15 GB headroom for this
model) — even though no new weight data is generated. Free up space first
if the create step fails with "no space left on device".

### Publishing to ollama.com

The build script only creates the model locally. Publishing so anyone can
`ollama pull` it directly (no local Ollama instance with the multimodal
model already pulled required) needs the `ollama` CLI with an
SSH-key-authenticated ollama.com account, run from a machine that has that
model loaded locally (e.g. the Ollama host itself):

```bash
ollama cp gemma4-text:26b-a4b-it-qat <your-username>/gemma4-text:26b-a4b-it-qat
ollama push <your-username>/gemma4-text:26b-a4b-it-qat
```

Account setup: [ollama.com/signup](https://ollama.com/signup). Add an SSH
public key under account settings before pushing — registry writes are
signed with that key, not a password/token.

**Important:** these commands must run on the host that already has the
model loaded (`ollama cp`/`ollama push` talk to whatever `OLLAMA_HOST`
points at — that server does the signing with its own key, not the
machine invoking the CLI). For a remote Ollama instance, run them there
directly rather than pointing a local CLI at it over the network.

Published build: **[ollama.com/lklimek/gemma4-text](https://ollama.com/lklimek/gemma4-text)**
(`lklimek/gemma4-text:26b-a4b-it-qat`) — pullable by anyone, no
prerequisite local model needed.

### Using it in MemCan

```bash
# .env
LLM_MODEL=lklimek/gemma4-text:26b-a4b-it-qat
```

No MemCan code or config schema changes needed — `LLM_MODEL` is already an
opaque string passed straight through to Ollama.

### Advanced: tuning for max context on a 16 GB card

MemCan itself doesn't need a large context window — its own recommended
Ollama settings (see `docker-compose.yml` / `.env.example` in the memcan
repo) are unrelated to this. But if you're running this model standalone
and want to push context length as high as it'll go while staying fully
GPU-resident on a 16 GB card, this is the validated combination from the
tuning work that produced this build:

```yaml
OLLAMA_CONTEXT_LENGTH: 80000
OLLAMA_KEEP_ALIVE: 10m
OLLAMA_NUM_PARALLEL: 1        # concurrent requests per model (default: 1)
OLLAMA_MAX_QUEUE: 1024        # queued requests before rejecting (default: 512)
OLLAMA_FLASH_ATTENTION: 1
OLLAMA_KV_CACHE_TYPE: q4_0
LLAMA_ARG_FIT_TARGET: 400     # llama.cpp's memory-fit safety margin, in MiB
```

Notes:
- `OLLAMA_FLASH_ATTENTION` + `OLLAMA_KV_CACHE_TYPE=q4_0` reduce per-token KV
  cache memory, which is what makes a large context window affordable at
  all — without them, 80k tokens of context won't fit alongside the model
  weights on 16 GB.
- `LLAMA_ARG_FIT_TARGET` overrides llama.cpp's default 1024 MiB free-memory
  safety margin (`common/arg.cpp` in llama.cpp; passed through by Ollama,
  which forwards its own environment to the spawned `llama-server`
  subprocess). Lowering it reclaims VRAM llama.cpp would otherwise hold back
  as headroom — 400 MiB was the validated safe floor for this card, not a
  universal number. Push it too low and you risk an OOM-triggered reload
  with a *larger* recalculated margin instead of a smaller one.
- `OLLAMA_NUM_PARALLEL: 1` disables concurrent request batching for this
  model — a deliberate memory-vs-throughput trade-off, since a second
  concurrent request would need its own KV cache allocation.
- This tuning combination is independent of the projector-stripping this
  build applies — it works equally well with the original
  `gemma4:26b-a4b-it-qat` if you want multimodal capability alongside a
  large context window. The projector removal just buys the initial VRAM
  headroom that makes room for this tuning in the first place.

### License & provenance

Apache License 2.0 (same as upstream Gemma 4). Original work:
`gemma4:26b-a4b-it-qat`, Copyright Google, Apache License 2.0. Full
modification notice (`NOTICE`), license text (`LICENSE`), and the
reproducible build script (`build_text_only.py`) are published in the
[`gemma4-text-26b-a4b-it-qat/`](https://github.com/lklimek/memcan/tree/main/ollama-models/gemma4-text-26b-a4b-it-qat)
subdirectory.
