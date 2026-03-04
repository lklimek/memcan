"""Tests for .env file discovery across config dir and CWD contexts."""

from __future__ import annotations

import textwrap
from pathlib import Path


def _write_env(path: Path, ollama_url: str = "http://test-host:11434") -> None:
    """Write a minimal .env file."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        textwrap.dedent(f"""\
        OLLAMA_URL={ollama_url}
        OLLAMA_API_KEY=test-key
    """)
    )


class TestFindEnvFile:
    """Test _find_env_file with explicit candidate lists."""

    def test_finds_first_existing(self, tmp_path):
        from mindojo_mcp.config import _find_env_file

        env_path = tmp_path / "config" / ".env"
        _write_env(env_path, "http://config-host:11434")

        result = _find_env_file([env_path, tmp_path / "other" / ".env"])
        assert result == env_path

    def test_skips_missing_returns_second(self, tmp_path):
        from mindojo_mcp.config import _find_env_file

        env_path = tmp_path / "fallback" / ".env"
        _write_env(env_path, "http://fallback:11434")

        result = _find_env_file([tmp_path / "nope" / ".env", env_path])
        assert result == env_path

    def test_returns_none_when_nothing_found(self, tmp_path):
        from mindojo_mcp.config import _find_env_file

        result = _find_env_file([tmp_path / "a" / ".env", tmp_path / "b" / ".env"])
        assert result is None

    def test_first_match_wins(self, tmp_path):
        from mindojo_mcp.config import _find_env_file

        first = tmp_path / "first" / ".env"
        second = tmp_path / "second" / ".env"
        _write_env(first, "http://first:11434")
        _write_env(second, "http://second:11434")

        result = _find_env_file([first, second])
        assert result == first


class TestDefaultCandidates:
    """Test that default candidate list includes config dir and CWD."""

    def test_config_dir_is_first_candidate(self):
        from mindojo_mcp.config import _CONFIG_DIR

        assert "mindojo" in str(_CONFIG_DIR)

    def test_cwd_env_used_when_config_dir_missing(self, tmp_path, monkeypatch):
        """CWD .env is used when platform config dir has no .env."""
        monkeypatch.chdir(tmp_path)
        _write_env(tmp_path / ".env", "http://cwd-host:11434")

        from mindojo_mcp.config import _find_env_file

        result = _find_env_file(
            [tmp_path / "nonexistent-config" / ".env", tmp_path / ".env"]
        )
        assert result == tmp_path / ".env"
