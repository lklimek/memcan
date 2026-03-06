"""Tests for extract_learnings SubagentStop hook."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest


class TestResolveProject:
    def test_project_from_git(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        repo = tmp_path / "my-project"
        repo.mkdir()
        with patch("subprocess.run") as mock_run:
            mock_run.return_value = type(
                "R", (), {"returncode": 0, "stdout": str(repo) + "\n"}
            )()
            assert _resolve_project(str(repo)) == "my-project"

    def test_git_failure_fallback(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        cwd = tmp_path / "fallback-repo"
        cwd.mkdir()
        with patch("subprocess.run") as mock_run:
            mock_run.return_value = type("R", (), {"returncode": 1, "stdout": ""})()
            assert _resolve_project(str(cwd)) == "fallback-repo"

    def test_global_scope_for_home_dir(self):
        from mindojo_mcp.extract_learnings import _resolve_project

        with patch("subprocess.run") as mock_run:
            mock_run.return_value = type("R", (), {"returncode": 1, "stdout": ""})()
            assert _resolve_project(str(Path.home())) is None

    def test_global_scope_for_tmp(self):
        from mindojo_mcp.extract_learnings import _resolve_project

        with patch("subprocess.run") as mock_run:
            mock_run.return_value = type("R", (), {"returncode": 1, "stdout": ""})()
            assert _resolve_project("/tmp") is None

    def test_global_scope_for_root(self):
        from mindojo_mcp.extract_learnings import _resolve_project

        with patch("subprocess.run") as mock_run:
            mock_run.return_value = type("R", (), {"returncode": 1, "stdout": ""})()
            assert _resolve_project("/") is None


class TestRun:
    @pytest.mark.asyncio
    async def test_skips_empty_message(self):
        from mindojo_mcp.extract_learnings import _run

        with patch(
            "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
        ) as mock_add:
            await _run({})
            mock_add.assert_not_called()
            await _run({"last_assistant_message": ""})
            mock_add.assert_not_called()

    @pytest.mark.asyncio
    async def test_skips_short_message(self):
        from mindojo_mcp.extract_learnings import _run

        with patch(
            "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
        ) as mock_add:
            await _run({"last_assistant_message": "x" * 99})
            mock_add.assert_not_called()

    @pytest.mark.asyncio
    async def test_calls_full_pipeline(self):
        from mindojo_mcp.extract_learnings import _run

        message = "A" * 200
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
            ) as mock_add,
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="myrepo",
            ),
        ):
            await _run({"last_assistant_message": message, "cwd": "/home/user/myrepo"})
            mock_add.assert_called_once_with(
                content=message,
                user_id="project:myrepo",
                metadata={"type": "lesson", "source": "auto-agent-stop"},
            )

    @pytest.mark.asyncio
    async def test_global_scope_when_no_project(self):
        from mindojo_mcp.extract_learnings import _run

        message = "B" * 200
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
            ) as mock_add,
            patch("mindojo_mcp.extract_learnings._resolve_project", return_value=None),
        ):
            await _run({"last_assistant_message": message, "cwd": "/home/user"})
            assert mock_add.call_args.kwargs["user_id"] == "global"


class TestMain:
    def test_survives_pipeline_failure(self):
        from mindojo_mcp.extract_learnings import main

        payload = json.dumps({"last_assistant_message": "C" * 200, "cwd": "/tmp/x"})
        with (
            patch("sys.stdin") as mock_stdin,
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
                side_effect=RuntimeError("boom"),
            ),
        ):
            mock_stdin.read.return_value = payload
            main()

    def test_survives_empty_stdin(self):
        from mindojo_mcp.extract_learnings import main

        with patch("sys.stdin") as mock_stdin:
            mock_stdin.read.return_value = ""
            main()

    def test_survives_invalid_json(self):
        from mindojo_mcp.extract_learnings import main

        with patch("sys.stdin") as mock_stdin:
            mock_stdin.read.return_value = "not json"
            main()
