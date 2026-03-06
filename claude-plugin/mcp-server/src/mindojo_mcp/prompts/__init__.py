"""Prompt templates for LLM-based memory operations.

Prompts are stored as .md files and loaded at import time.
"""

from datetime import datetime
from pathlib import Path
from string import Template

_PROMPT_DIR = Path(__file__).parent


def _load(name: str, **kwargs: str) -> str:
    """Load a prompt .md file and apply $-style substitutions."""
    text = (_PROMPT_DIR / name).read_text()
    return Template(text).safe_substitute(**kwargs) if kwargs else text


_today = datetime.now().strftime("%Y-%m-%d")

FACT_EXTRACTION_PROMPT = _load("fact-extraction.md", today=_today)

FACT_EXTRACTION_HOOK_PROMPT = _load("fact-extraction-hook.md", today=_today)

MEMORY_UPDATE_PROMPT = _load("memory-update.md")
