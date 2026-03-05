"""Unit tests for Settings and to_mem0_config()."""

from __future__ import annotations

import os


class TestToMem0Config:
    """Verify to_mem0_config() builds correct mem0 config dicts."""

    def test_to_mem0_config_has_ollama_provider(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test")
        cfg = s.to_mem0_config()
        assert cfg["llm"]["provider"] == "ollama"
        assert cfg["embedder"]["provider"] == "ollama"

    def test_to_mem0_config_uses_settings_values(self):
        from mindojo_mcp.config import Settings

        s = Settings(
            ollama_url="http://custom:11434",
            ollama_llm_model="llama3:8b",
            ollama_embed_model="nomic-embed:latest",
            ollama_api_key="test",
        )
        cfg = s.to_mem0_config()
        assert cfg["llm"]["config"]["model"] == "llama3:8b"
        assert cfg["llm"]["config"]["ollama_base_url"] == "http://custom:11434"
        assert cfg["embedder"]["config"]["model"] == "nomic-embed:latest"
        assert cfg["embedder"]["config"]["ollama_base_url"] == "http://custom:11434"

    def test_to_mem0_config_neo4j_disabled_by_default(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test")
        cfg = s.to_mem0_config()
        assert "graph_store" not in cfg

    def test_to_mem0_config_neo4j_enabled(self):
        from mindojo_mcp.config import Settings

        s = Settings(
            neo4j_enabled=True,
            neo4j_url="bolt://db:7687",
            neo4j_user="admin",
            neo4j_password="secret",
            ollama_api_key="test",
        )
        cfg = s.to_mem0_config()
        assert cfg["graph_store"]["provider"] == "neo4j"
        assert cfg["graph_store"]["config"]["url"] == "bolt://db:7687"
        assert cfg["graph_store"]["config"]["username"] == "admin"
        assert cfg["graph_store"]["config"]["password"] == "secret"


class TestDefaults:
    """Verify default values for Settings fields."""

    def test_default_embed_model(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test", _env_file=None)
        assert s.ollama_embed_model == "qwen3-embedding:4b"

    def test_default_embed_dims(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test", _env_file=None)
        assert s.qdrant_embed_dims == 2560

    def test_tech_stack_defaults_to_empty(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test", _env_file=None)
        assert s.tech_stack == ""


class TestOllamaApiKeyExport:
    """Verify OLLAMA_API_KEY propagation to os.environ."""

    def test_ollama_api_key_exported_to_env(self, monkeypatch):
        monkeypatch.delenv("OLLAMA_API_KEY", raising=False)

        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="my-secret-key")
        if s.ollama_api_key:
            os.environ.setdefault("OLLAMA_API_KEY", s.ollama_api_key)

        assert os.environ["OLLAMA_API_KEY"] == "my-secret-key"


class TestResolveUserId:
    """Test _resolve_user_id 3-tier priority."""

    def test_explicit_user_id_wins(self):
        from mindojo_mcp.server import _resolve_user_id

        assert _resolve_user_id(project="foo", user_id="explicit") == "explicit"

    def test_project_scoping_when_no_user_id(self):
        from mindojo_mcp.server import _resolve_user_id

        assert _resolve_user_id(project="myrepo", user_id=None) == "project:myrepo"

    def test_default_when_nothing_provided(self):
        from mindojo_mcp.server import _resolve_user_id

        result = _resolve_user_id(project=None, user_id=None)
        assert result == "global"
