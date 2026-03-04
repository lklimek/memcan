"""Configuration for MindAJO MCP server.

Searches for .env in platform-appropriate config dir, then CWD, then defaults.
Environment variables always override .env values.
"""

from __future__ import annotations

import os
from pathlib import Path

from platformdirs import user_config_dir
from pydantic_settings import BaseSettings, SettingsConfigDict

_CONFIG_DIR = Path(user_config_dir("mindajo"))


def _find_env_file(candidates: list[Path] | None = None) -> Path | None:
    """Return the first existing .env from candidate paths.

    Default search order:
      1. Platform config dir  (~/.config/mindajo/.env on Linux)
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
    ollama_llm_model: str = "qwen2.5:14b"
    ollama_embed_model: str = "qwen3-embedding:8b"

    # Qdrant
    qdrant_url: str = "http://localhost:6333"
    qdrant_collection: str = "mindajo"
    qdrant_embed_dims: int = 4096

    # Neo4j (optional)
    neo4j_enabled: bool = False
    neo4j_url: str = "bolt://localhost:7687"
    neo4j_user: str = "neo4j"
    neo4j_password: str = "changeme"

    # Defaults
    default_user_id: str = "global"

    def to_mem0_config(self) -> dict:
        """Build mem0 Memory config dict from settings."""
        config: dict = {
            "llm": {
                "provider": "ollama",
                "config": {
                    "model": self.ollama_llm_model,
                    "ollama_base_url": self.ollama_url,
                },
            },
            "embedder": {
                "provider": "ollama",
                "config": {
                    "model": self.ollama_embed_model,
                    "ollama_base_url": self.ollama_url,
                },
            },
            "vector_store": {
                "provider": "qdrant",
                "config": {
                    "collection_name": self.qdrant_collection,
                    "url": self.qdrant_url,
                    "embedding_model_dims": self.qdrant_embed_dims,
                },
            },
        }

        if self.neo4j_enabled:
            config["graph_store"] = {
                "provider": "neo4j",
                "config": {
                    "url": self.neo4j_url,
                    "username": self.neo4j_user,
                    "password": self.neo4j_password,
                },
            }

        return config


settings = Settings()

# Export OLLAMA_API_KEY to process env so the ollama Python client picks it up.
# The client reads os.getenv("OLLAMA_API_KEY") directly — Pydantic settings alone
# doesn't propagate .env values to the OS environment.
if settings.ollama_api_key:
    os.environ.setdefault("OLLAMA_API_KEY", settings.ollama_api_key)
