"""Hook handler for SubagentStop and PreCompact events.

CLI entry point invoked async by Claude Code on SubagentStop and PreCompact
events.  Reads JSON from stdin, dispatches to the appropriate handler, and
calls the full memory pipeline (extract -> dedup -> store).
"""

from __future__ import annotations

import asyncio
import json
import logging
import subprocess
import sys
from pathlib import Path, PurePosixPath

from .config import settings
from .memory_pipeline import do_add_memory

logger = logging.getLogger(__name__)

_MIN_MESSAGE_LENGTH = 300


def _repo_name_from_url(url: str) -> str | None:
    """Extract repository name from a git remote URL.

    Handles HTTPS, SSH protocol, and SSH shorthand (git@host:user/repo.git).
    """
    url = url.strip()
    if not url:
        return None

    # SSH shorthand: git@github.com:user/repo.git
    if ":" in url and not url.startswith(("https://", "http://", "ssh://")):
        _, _, path_part = url.partition(":")
        url = path_part

    name = PurePosixPath(url).name
    if name.endswith(".git"):
        name = name[:-4]
    return name or None


def _resolve_project(cwd: str) -> str | None:
    """Determine project name from cwd via git remote, with fallbacks.

    Priority:
      1. git remote get-url origin -> parse repo name from URL
      2. git rev-parse --show-toplevel -> basename (no remote configured)
      3. cwd basename (not inside a git repo at all)
    """
    # 1. Try git remote origin URL
    try:
        result = subprocess.run(
            ["git", "-C", cwd, "remote", "get-url", "origin"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0:
            name = _repo_name_from_url(result.stdout)
            if name and name not in {"tmp", "temp"}:
                return name
    except (OSError, subprocess.TimeoutExpired):
        pass

    # 2. Fallback: git toplevel basename
    try:
        result = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0:
            basename = Path(result.stdout.strip()).name
            if basename and basename not in {"tmp", "temp", "/"}:
                return basename
    except (OSError, subprocess.TimeoutExpired):
        pass

    # 3. Fallback: cwd basename
    cwd_path = Path(cwd).resolve()
    if cwd_path == Path.home().resolve():
        return None
    if len(cwd_path.parts) <= 2:
        return None
    basename = cwd_path.name
    if basename.lower() in {"tmp", "temp"}:
        return None
    return basename or None


def _extract_text_from_transcript_line(line_obj: dict) -> str | None:
    """Extract assistant text content from a transcript JSONL line.

    Returns concatenated text blocks, or None if no text content found.
    """
    if line_obj.get("type") != "assistant":
        return None

    message = line_obj.get("message")
    if not isinstance(message, dict):
        return None
    if message.get("role") != "assistant":
        return None

    content = message.get("content")
    if not isinstance(content, list):
        return None

    texts = [
        block["text"]
        for block in content
        if isinstance(block, dict) and block.get("type") == "text" and block.get("text")
    ]
    return "".join(texts) if texts else None


async def _run(payload: dict) -> None:
    """Extract learnings from agent output via the full memory pipeline."""
    message = payload.get("last_assistant_message", "")
    if not message or len(message) < _MIN_MESSAGE_LENGTH:
        logger.info(
            "SubagentStop hook: message too short (%d chars), skipping",
            len(message or ""),
        )
        return

    cwd = payload.get("cwd", "")
    project = _resolve_project(cwd) if cwd else None
    user_id = f"project:{project}" if project else settings.default_user_id
    logger.info("SubagentStop hook: project=%s, user_id=%s", project, user_id)

    metadata = {"type": "lesson", "source": "auto-agent-stop"}
    logger.info(
        "SubagentStop hook: sending to memory pipeline, content_len=%d", len(message)
    )
    await do_add_memory(content=message, user_id=user_id, metadata=metadata)
    logger.info("SubagentStop hook: memory pipeline complete")


async def _run_precompact(payload: dict) -> None:
    """Extract learnings from transcript before compaction."""
    transcript_path = payload.get("transcript_path", "")
    if not transcript_path:
        logger.info("PreCompact hook: no transcript_path, skipping")
        return

    path = Path(transcript_path)
    if not path.is_file():
        logger.info("PreCompact hook: transcript not found: %s", transcript_path)
        return

    try:
        raw_lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        logger.error("PreCompact hook: failed to read transcript: %s", exc)
        return

    logger.info(
        "PreCompact hook: transcript=%s, line_count=%d", transcript_path, len(raw_lines)
    )

    last_text: str | None = None
    for raw in raw_lines:
        raw = raw.strip()
        if not raw:
            continue
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError:
            continue
        text = _extract_text_from_transcript_line(obj)
        if text:
            last_text = text

    if not last_text or len(last_text) < _MIN_MESSAGE_LENGTH:
        logger.info(
            "PreCompact hook: last assistant message too short (%d chars), skipping",
            len(last_text or ""),
        )
        return

    logger.info("PreCompact hook: extracted message len=%d", len(last_text))

    cwd = payload.get("cwd", "")
    project = _resolve_project(cwd) if cwd else None
    user_id = f"project:{project}" if project else settings.default_user_id
    logger.info("PreCompact hook: project=%s, user_id=%s", project, user_id)

    metadata = {"type": "lesson", "source": "auto-pre-compact"}
    await do_add_memory(content=last_text, user_id=user_id, metadata=metadata)
    logger.info("PreCompact hook: memory pipeline complete")


def main() -> None:
    """CLI entry point -- reads stdin, dispatches by hook type. Never crashes."""
    log_dir = Path.home() / ".claude" / "logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    file_handler = logging.FileHandler(log_dir / "mindojo-hooks.log")
    file_handler.setLevel(logging.INFO)
    file_handler.setFormatter(
        logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s")
    )
    root_logger = logging.getLogger()
    root_logger.setLevel(logging.INFO)
    root_logger.addHandler(file_handler)

    try:
        raw = sys.stdin.read()
        logger.info("Hook invoked, stdin=%d bytes", len(raw))
        if not raw.strip():
            logger.info("Hook: no input, exiting")
            return
        payload = json.loads(raw)
        hook_event = payload.get("hook_event_name", "")
        logger.info("Hook: event=%s, keys=%s", hook_event, list(payload.keys()))

        if hook_event == "SubagentStop":
            asyncio.run(_run(payload))
        elif hook_event == "PreCompact":
            asyncio.run(_run_precompact(payload))
        else:
            logger.info("Hook: unhandled event %r, skipping", hook_event)
    except Exception:
        logger.error("extract_learnings hook failed", exc_info=True)
