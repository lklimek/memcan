"""Configuration for AI Brain MCP server.

Reads .env file from repo root, then environment variables (env vars override).
"""

from __future__ import annotations

from pathlib import Path

from pydantic_settings import BaseSettings, SettingsConfigDict

# Repo root is two levels up from mcp-server/src/ai_brain_mcp/
_REPO_ROOT = Path(__file__).resolve().parent.parent.parent.parent


class Settings(BaseSettings):
    """MCP server settings from .env file + environment variables."""

    model_config = SettingsConfigDict(
        env_file=str(_REPO_ROOT / ".env"),
        env_file_encoding="utf-8",
        extra="ignore",  # ignore vars not in this model (e.g. TRAEFIK_AUTH)
    )

    # Ollama
    ollama_url: str = "http://localhost:11434"
    ollama_llm_model: str = "qwen2.5:14b"
    ollama_embed_model: str = "nomic-embed-text"

    # Qdrant
    qdrant_url: str = "http://localhost:6333"
    qdrant_collection: str = "ai_brain"
    qdrant_embed_dims: int = 768

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
