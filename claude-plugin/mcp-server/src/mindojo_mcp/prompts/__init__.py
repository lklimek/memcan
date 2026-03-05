"""Custom prompts for mem0 integration.

Replaces mem0's default USER_MEMORY_EXTRACTION_PROMPT which is designed for
personal assistant use cases ("Personal Information Organizer").  That prompt
causes some models (e.g. qwen3.5:9b) to discard technical facts because they
don't look like "personal preferences".

Prompts are stored as .md files and loaded at import time.
Injected via mem0's custom_fact_extraction_prompt config key.
"""

from datetime import datetime
from pathlib import Path
from string import Template

_PROMPT_DIR = Path(__file__).parent


def _load(name: str, **kwargs: str) -> str:
    """Load a prompt .md file and apply $-style substitutions."""
    text = (_PROMPT_DIR / name).read_text()
    return Template(text).safe_substitute(**kwargs) if kwargs else text


FACT_EXTRACTION_PROMPT = _load(
    "fact-extraction.md",
    today=datetime.now().strftime("%Y-%m-%d"),
)
