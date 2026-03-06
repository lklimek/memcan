"""Configuration for MindOJO MCP server.

Searches for .env in platform-appropriate config dir, then CWD, then defaults.
Environment variables always override .env values.
"""

from __future__ import annotations

import logging
import os
from pathlib import Path

from platformdirs import user_config_dir
from pydantic_settings import BaseSettings, SettingsConfigDict

_CONFIG_DIR = Path(user_config_dir("mindojo"))

# Hardcoded model and collection constants — change here, not in .env.
LLM_MODEL = "qwen3.5:4b"
EMBED_MODEL = "qwen3-embedding:4b"
EMBED_DIMS = 2560
QDRANT_COLLECTION = "mindojo-memories"
STANDARDS_COLLECTION = "mindojo-standards"
CODE_COLLECTION = "mindojo-code"

# Model for metadata extraction in indexing scripts (not the main LLM)
EXTRACTION_MODEL = "qwen3.5:4b"


def _find_env_file(candidates: list[Path] | None = None) -> Path | None:
    """Return the first existing .env from candidate paths.

    Default search order:
      1. Platform config dir  (~/.config/mindojo/.env on Linux)
      2. CWD .env             (development — running from source checkout)
    """
    if candidates is None:
        candidates = [
            _CONFIG_DIR / ".env",
            Path.cwd() / ".env",
        ]
    for p in candidates:
        if p.is_file():
            return p
    return None


_env_file = _find_env_file()


class Settings(BaseSettings):
    """MCP server settings from .env file + environment variables."""

    model_config = SettingsConfigDict(
        env_file=str(_env_file) if _env_file else None,
        env_file_encoding="utf-8",
        extra="ignore",  # ignore vars not in this model (e.g. TRAEFIK_AUTH)
    )

    # Ollama
    ollama_url: str = "http://localhost:11434"
    ollama_api_key: str = ""

    # Qdrant
    qdrant_url: str = "http://localhost:6333"

    # Defaults
    default_user_id: str = "global"
    tech_stack: str = ""  # e.g. "rust", "python", "react"; empty = none/mixed

    # Memory distillation (LLM fact extraction + dedup)
    distill_memories: bool = True

    # Logging — defaults to ~/.claude/logs/mindojo-mcp.log
    log_file: str = str(Path.home() / ".claude" / "logs" / "mindojo-mcp.log")


settings = Settings()


logger = logging.getLogger(__name__)


async def ensure_models(
    ollama_url: str | None = None,
) -> None:
    """Pull configured Ollama models if not already present.

    Checks both LLM and embedding models. Skips models that already exist.
    Called once during lazy init.
    """
    from ollama import AsyncClient, ResponseError

    url = ollama_url or settings.ollama_url
    client = AsyncClient(host=url)
    for model in (LLM_MODEL, EMBED_MODEL):
        try:
            await client.show(model)
            logger.debug("Model %s already available", model)
        except ResponseError:
            logger.info("Pulling model %s …", model)
            try:
                await client.pull(model)
                logger.info("Model %s pulled successfully", model)
            except ResponseError as exc:
                logger.error("Failed to pull %s: %s", model, exc)
                raise


# Export OLLAMA_API_KEY to process env so the ollama Python client picks it up.
# The client reads os.getenv("OLLAMA_API_KEY") directly — Pydantic settings alone
# doesn't propagate .env values to the OS environment.
if settings.ollama_api_key:
    os.environ.setdefault("OLLAMA_API_KEY", settings.ollama_api_key)
