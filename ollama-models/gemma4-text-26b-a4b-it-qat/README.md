# gemma4-text:26b-a4b-it-qat — projector-free build

A text-only variant of Ollama's `gemma4:26b-a4b-it-qat` with the bundled
vision projector removed. Same text weights, byte-identical — just without
the ~1.2 GB multimodal component MemCan never uses.

## Why this exists

MemCan is text-only — it never sends images. But `gemma4:26b-a4b-it-qat`
ships with a vision projector (`gemma4v`) that Ollama loads and tries to
GPU-offload on every startup, regardless of whether the client ever uses it.

On VRAM-constrained cards this has a real cost: Ollama's memory-fit
calculation reserves headroom for the projector even when it ends up on
CPU, which can push several of the *text* model's own MoE-expert layers off
GPU too — even though the text model alone would have fit entirely in VRAM.
See `docs/memcan-model-guide.html` in the repo root for the full VRAM
picture; this directory is the fix for the specific "why isn't this at
100% GPU" problem on borderline hardware (e.g. a 16 GB card).

Measured effect on one such card (RTX 5060 Ti, 16 GB): the stock model
landed at ~87% GPU residency (partial CPU offload); this build landed at
**100%** — and cold-load time dropped from 56-75s to ~7.6s.

## What was changed

Nothing about the text model itself. `build_text_only.py` reads the
existing model's manifest, finds the text-model blob's digest (leaving the
projector's digest behind), and asks Ollama to create a new model from just
that blob. The weights are reused unmodified — this is a manifest-level
change, not a re-quantization or re-training.

See `NOTICE` for the formal Apache 2.0 modification notice, and `LICENSE`
for the full license text (both required for redistribution under Apache
2.0, which is what Gemma 4 ships under).

## Building it yourself

Requires only HTTP access to an Ollama instance that already has
`gemma4:26b-a4b-it-qat` pulled — no shell access to that host needed for
this step.

```bash
uv run python build_text_only.py \
    --host http://<ollama-host>:11434 \
    --api-key "$OLLAMA_API_KEY" \
    --base gemma4:26b-a4b-it-qat \
    --new gemma4-text:26b-a4b-it-qat
```

The script fetches the manifest, extracts the text-model blob digest,
creates the new model, then loads it and reports the resulting GPU
residency percentage so you can confirm it actually landed at 100% on your
hardware before publishing anything.

**Note:** Ollama's `/api/create` re-validates the referenced blob during
its "verifying conversion" step, which needs roughly the model's own size
again in free disk space on the Ollama host (~15 GB headroom for this
model) — even though no new weight data is generated. Free up space first
if the create step fails with "no space left on device".

## Publishing to ollama.com

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

## Using it in MemCan

```bash
# .env
LLM_MODEL=lklimek/gemma4-text:26b-a4b-it-qat
```

No MemCan code or config schema changes needed — `LLM_MODEL` is already an
opaque string passed straight through to Ollama.
