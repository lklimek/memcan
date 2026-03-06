"""SubagentStop hook: extract learnings from agent output and store in Qdrant.

CLI entry point invoked async by Claude Code on SubagentStop events.
Reads JSON from stdin, calls the full memory pipeline (extract -> dedup -> store).
"""

from __future__ import annotations

import asyncio
import json
import logging
import subprocess
import sys
from pathlib import Path

from .config import settings
from .memory_pipeline import do_add_memory

logger = logging.getLogger(__name__)

_MIN_MESSAGE_LENGTH = 100


def _resolve_project(cwd: str) -> str | None:
    """Determine project name from cwd via git, with fallbacks."""
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

    cwd_path = Path(cwd).resolve()
    if cwd_path == Path.home().resolve():
        return None
    if len(cwd_path.parts) <= 2:
        return None
    basename = cwd_path.name
    if basename.lower() in {"tmp", "temp"}:
        return None
    return basename or None


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


def main() -> None:
    """CLI entry point -- reads stdin, calls pipeline. Never crashes."""
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
        logger.info("SubagentStop hook invoked, stdin=%d bytes", len(raw))
        if not raw.strip():
            logger.info("SubagentStop hook: no input, exiting")
            return
        payload = json.loads(raw)
        logger.info("SubagentStop hook: payload keys=%s", list(payload.keys()))
        asyncio.run(_run(payload))
    except Exception:
        logger.error("extract_learnings hook failed", exc_info=True)
