"""Unit tests for Settings."""

from __future__ import annotations

import os


class TestDistillMemories:
    """Verify distill_memories setting."""

    def test_distill_memories_defaults_to_true(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test", _env_file=None)
        assert s.distill_memories is True

    def test_distill_memories_can_be_disabled(self):
        from mindojo_mcp.config import Settings

        s = Settings(ollama_api_key="test", distill_memories=False, _env_file=None)
        assert s.distill_memories is False


class TestConstants:
    """Verify hardcoded model and collection constants."""

    def test_model_constants(self):
        from mindojo_mcp.config import (
            EMBED_DIMS,
            EMBED_MODEL,
            LLM_MODEL,
            QDRANT_COLLECTION,
        )

        assert LLM_MODEL == "qwen3.5:4b"
        assert EMBED_MODEL == "qwen3-embedding:4b"
        assert EMBED_DIMS == 2560
        assert QDRANT_COLLECTION == "mindojo-memories"

    def test_new_collection_constants(self):
        from mindojo_mcp.config import (
            CODE_COLLECTION,
            EXTRACTION_MODEL,
            STANDARDS_COLLECTION,
        )

        assert STANDARDS_COLLECTION == "mindojo-standards"
        assert CODE_COLLECTION == "mindojo-code"
        assert EXTRACTION_MODEL == "qwen3.5:4b"

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
