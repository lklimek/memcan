#!/usr/bin/env python3
"""Replay hook data log against a different prompt/model for prompt tuning.

Reads the JSONL hook data log and re-runs fact extraction with a specified
prompt and model, comparing results to the logged decisions.

Usage:
    uv run scripts/test_classification.py --prompt PATH --model MODEL [--data PATH] [--ollama-url URL]
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from datetime import datetime
from pathlib import Path
from string import Template

import httpx

_DEFAULT_DATA = Path.home() / ".claude" / "logs" / "mindojo-hook-data.jsonl"
_DEFAULT_OLLAMA_URL = "http://localhost:11434"


def _load_env(repo_root: Path) -> dict[str, str]:
    """Parse key=value pairs from .env file (no shell expansion)."""
    env_path = repo_root / ".env"
    if not env_path.is_file():
        return {}
    result: dict[str, str] = {}
    for line in env_path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        value = value.strip().strip("\"'")
        if key:
            result[key] = value
    return result


def _call_ollama(
    url: str,
    model: str,
    system_prompt: str,
    content: str,
    *,
    api_key: str = "",
    max_attempts: int = 3,
    timeout: float = 120.0,
) -> list[str] | None:
    """Send content to Ollama and parse facts. Returns None on failure."""
    headers: dict[str, str] = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"

    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": content},
        ],
        "stream": False,
        "format": "json",
        "options": {"num_predict": 1024},
        "think": False,
    }

    for attempt in range(1, max_attempts + 1):
        try:
            resp = httpx.post(
                f"{url}/api/chat",
                json=payload,
                headers=headers,
                timeout=timeout,
            )
            resp.raise_for_status()
            data = resp.json()
            text = data.get("message", {}).get("content", "")
            parsed = json.loads(text)
            return parsed.get("facts", [])
        except (httpx.TimeoutException, httpx.ConnectError) as exc:
            if attempt < max_attempts:
                print(
                    f"  Attempt {attempt}/{max_attempts} failed ({exc}), retrying in 5s..."
                )
                time.sleep(5)
            else:
                print(f"  All {max_attempts} attempts failed: {exc}")
                return None
        except (json.JSONDecodeError, KeyError) as exc:
            print(f"  Parse error: {exc}")
            return None
        except httpx.HTTPStatusError as exc:
            print(f"  HTTP error: {exc}")
            return None
    return None


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Replay hook data log with a different prompt/model"
    )
    parser.add_argument(
        "--prompt",
        required=True,
        type=Path,
        help="Path to fact-extraction prompt .md file",
    )
    parser.add_argument(
        "--model", required=True, help="Ollama model name (e.g. qwen3.5:4b)"
    )
    parser.add_argument(
        "--data",
        type=Path,
        default=_DEFAULT_DATA,
        help=f"Path to JSONL data file (default: {_DEFAULT_DATA})",
    )
    parser.add_argument(
        "--ollama-url",
        default=None,
        help="Ollama API URL (default: from .env or localhost)",
    )
    parser.add_argument(
        "--ollama-api-key", default=None, help="Ollama API key (default: from .env)"
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    env_vars = _load_env(repo_root)

    ollama_url = args.ollama_url or env_vars.get("OLLAMA_URL", _DEFAULT_OLLAMA_URL)
    api_key = args.ollama_api_key or env_vars.get("OLLAMA_API_KEY", "")

    if not args.prompt.is_file():
        print(f"Error: prompt file not found: {args.prompt}", file=sys.stderr)
        sys.exit(1)

    if not args.data.is_file():
        print(f"Error: data file not found: {args.data}", file=sys.stderr)
        sys.exit(1)

    prompt_template = args.prompt.read_text()
    today = datetime.now().strftime("%Y-%m-%d")
    system_prompt = Template(prompt_template).safe_substitute(today=today)

    entries = []
    for line in args.data.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
            decision = entry.get("decision", "")
            if decision in ("kept", "rejected"):
                entries.append(entry)
        except json.JSONDecodeError:
            continue

    if not entries:
        print("No testable entries found (need 'kept' or 'rejected' decisions)")
        sys.exit(0)

    print(f"Loaded {len(entries)} testable entries from {args.data}")
    print(f"Model: {args.model}")
    print(f"Prompt: {args.prompt}")
    print(f"Ollama: {ollama_url}")
    print()

    tp = fp = tn = fn = 0

    for i, entry in enumerate(entries, 1):
        logged_decision = entry["decision"]
        content = entry["content"]
        expect_facts = logged_decision == "kept"

        new_facts = _call_ollama(
            url=ollama_url,
            model=args.model,
            system_prompt=system_prompt,
            content=content,
            api_key=api_key,
        )

        if new_facts is None:
            status = "ERROR"
            label = "---"
        else:
            got_facts = len(new_facts) > 0
            if expect_facts and got_facts:
                tp += 1
                status = "TP"
                label = "KEPT"
            elif expect_facts and not got_facts:
                fn += 1
                status = "FN"
                label = "KEPT"
            elif not expect_facts and not got_facts:
                tn += 1
                status = "TN"
                label = "REJECTED"
            else:
                fp += 1
                status = "FP"
                label = "REJECTED"

        prefix = content[:60].replace("\n", " ")
        n_facts = len(new_facts) if new_facts is not None else "?"
        print(f"[{i}/{len(entries)}] {status} {label} facts={n_facts} | {prefix}...")

    print()
    print("=" * 60)
    total = tp + fp + tn + fn
    accuracy = (tp + tn) / total * 100 if total else 0
    precision = tp / (tp + fp) * 100 if (tp + fp) else 0
    recall = tp / (tp + fn) * 100 if (tp + fn) else 0

    print(f"Total entries:  {total}")
    print(f"True Positive:  {tp}")
    print(f"False Positive: {fp}")
    print(f"True Negative:  {tn}")
    print(f"False Negative: {fn}")
    print(f"Accuracy:       {accuracy:.1f}%")
    print(f"Precision:      {precision:.1f}%")
    print(f"Recall:         {recall:.1f}%")


if __name__ == "__main__":
    main()
