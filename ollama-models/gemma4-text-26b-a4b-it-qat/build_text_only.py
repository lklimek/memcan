#!/usr/bin/env python3
"""Build a projector-free (text-only) variant of a multimodal Ollama model.

Why: some Ollama-packaged models (e.g. gemma4:26b-a4b-it-qat) bundle a vision
projector that MemCan never uses — MemCan is text-only. Ollama loads and tries
to GPU-offload the projector unconditionally whenever a model has one, which
can force partial CPU offload of the *main* model on VRAM-constrained cards
even when the text model alone would fit entirely on GPU. Stripping the
projector layer removes that overhead with zero impact on the text model's
own weights (they are reused byte-identical from the source model's blob,
not re-quantized or retrained).

Talks only to the Ollama HTTP API (POST /api/show, POST /api/create) — no
shell access to the Ollama host is needed for this step. The final publish
step (`ollama cp` + `ollama push`) requires the `ollama` CLI with an
authenticated SSH key and is printed at the end, not run by this script.

Usage:
    uv run python build_text_only.py \\
        --host http://192.168.x.x:11434 \\
        --api-key "$OLLAMA_API_KEY" \\
        [--base gemma4:26b-a4b-it-qat] [--new gemma4-text:26b-a4b-it-qat]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.error
import urllib.request


def _headers(api_key: str | None) -> dict:
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    return headers


def api_call(host: str, api_key: str | None, path: str, payload: dict, timeout: int = 60) -> dict:
    """POST JSON to the Ollama API, return the single parsed JSON response."""
    req = urllib.request.Request(
        f"{host.rstrip('/')}{path}",
        data=json.dumps(payload).encode(),
        method="POST",
        headers=_headers(api_key),
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def api_stream(host: str, api_key: str | None, path: str, payload: dict, timeout: int = 300) -> list[dict]:
    """POST JSON to the Ollama API, return the list of streamed NDJSON status objects."""
    req = urllib.request.Request(
        f"{host.rstrip('/')}{path}",
        data=json.dumps(payload).encode(),
        method="POST",
        headers=_headers(api_key),
    )
    events = []
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        for raw_line in resp:
            line = raw_line.strip()
            if not line:
                continue
            event = json.loads(line)
            events.append(event)
            print(f"    {event}")
            if "error" in event:
                raise RuntimeError(f"Ollama API error: {event['error']}")
    return events


def api_get(host: str, api_key: str | None, path: str, timeout: int = 60) -> dict:
    req = urllib.request.Request(f"{host.rstrip('/')}{path}", headers=_headers(api_key))
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def parse_params(parameters_text: str) -> dict:
    """Parse /api/show's flat "key value" parameters block into a dict."""
    params = {}
    for line in parameters_text.splitlines():
        parts = line.split(None, 1)
        if len(parts) != 2:
            continue
        key, value = parts
        for cast in (int, float):
            try:
                value = cast(value)
                break
            except ValueError:
                continue
        params[key] = value
    return params


def extract_model_digest(modelfile: str) -> str:
    """Return the sha256 digest of the *text-model* FROM line.

    Convention observed across every multimodal Ollama manifest checked so
    far: the main model layer is listed before the projector layer. If that
    ever changes upstream, this will pick the wrong blob — the post-create
    GPU-residency check at the end of main() exists precisely to catch that
    class of mistake before anyone publishes a broken model.
    """
    froms = re.findall(r"^FROM\s+(\S+)", modelfile, re.MULTILINE)
    if len(froms) < 2:
        raise RuntimeError(
            f"expected 2+ FROM lines (model + projector), found {len(froms)} — "
            "nothing to strip, or manifest shape has changed"
        )
    path = froms[0]
    digest = path.rsplit("sha256-", 1)[-1]
    return f"sha256:{digest}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True, help="Ollama base URL, e.g. http://host:11434")
    parser.add_argument("--api-key", default=None, help="Bearer token, if the endpoint requires one")
    parser.add_argument("--base", default="gemma4:26b-a4b-it-qat", help="Source (multimodal) model")
    parser.add_argument("--new", default="gemma4-text:26b-a4b-it-qat", help="New text-only model name")
    args = parser.parse_args()

    print(f"==> Fetching manifest for {args.base}")
    show = api_call(args.host, args.api_key, "/api/show", {"name": args.base})

    if "projector_info" not in show:
        print(f"==> {args.base} has no projector layer — nothing to strip.")
        return 0

    digest = extract_model_digest(show["modelfile"])
    print(f"==> Text-model blob digest: {digest}")

    print(f"==> Creating {args.new} (text-only, no projector)")
    create_payload = {
        "model": args.new,
        "files": {"model.gguf": digest},
        "template": show["template"],
        "parameters": parse_params(show.get("parameters", "")),
    }
    events = api_stream(args.host, args.api_key, "/api/create", create_payload)
    if not events or events[-1].get("status") != "success":
        raise RuntimeError(f"create did not report success; last event: {events[-1] if events else None}")

    print(f"\n==> Verifying: loading {args.new} and checking GPU residency")
    api_call(
        args.host, args.api_key, "/api/generate",
        {"model": args.new, "prompt": "hi", "stream": False},
        timeout=180,
    )

    ps = api_get(args.host, args.api_key, "/api/ps")
    match = next((m for m in ps.get("models", []) if m["name"] == args.new), None)
    if match is None:
        print("WARNING: model not found in /api/ps after load — cannot verify residency.")
    else:
        size, vram = match["size"], match["size_vram"]
        pct = 100 * vram / size if size else 0
        print(f"    {vram}/{size} bytes resident ({pct:.1f}% GPU)")
        if pct >= 99.9:
            print("    Fully GPU-resident.")
        else:
            print("    NOTE: not fully GPU-resident — check available VRAM on this host.")

    print(
        f"\n==> Done. Publish it (run locally, with an authenticated `ollama` CLI\n"
        f"    and SSH key configured on ollama.com — see ../README.md):\n\n"
        f"    ollama cp {args.new} <your-ollama-username>/{args.new}\n"
        f"    ollama push <your-ollama-username>/{args.new}\n\n"
        f"    Then point MemCan at it: LLM_MODEL=<your-ollama-username>/{args.new}\n"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, urllib.error.URLError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
