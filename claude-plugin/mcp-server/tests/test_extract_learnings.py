"""Tests for extract_learnings hook (SubagentStop + PreCompact)."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

# Default JSONL log path used in tests (overridden via env/patch)
_HOOK_DATA_LOG = "mindojo-hook-data.jsonl"


# ---------------------------------------------------------------------------
# _resolve_project
# ---------------------------------------------------------------------------


def _mock_run_result(returncode: int, stdout: str = "") -> MagicMock:
    """Helper to create a mock subprocess.run return value."""
    return MagicMock(returncode=returncode, stdout=stdout)


class TestResolveProject:
    def test_project_from_remote_https(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        repo = tmp_path / "whatever-worktree"
        repo.mkdir()
        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(
                0, "https://github.com/user/mindojo.git\n"
            )
            assert _resolve_project(str(repo)) == "mindojo"
            mock_run.assert_called_once()
            args = mock_run.call_args[0][0]
            assert "remote" in args and "get-url" in args

    def test_project_from_remote_ssh(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        repo = tmp_path / "agent-a9ba48be"
        repo.mkdir()
        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(
                0, "git@github.com:user/dash-evo-tool.git\n"
            )
            assert _resolve_project(str(repo)) == "dash-evo-tool"

    def test_project_from_remote_no_git_suffix(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        repo = tmp_path / "clone"
        repo.mkdir()
        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(
                0, "https://github.com/user/repo\n"
            )
            assert _resolve_project(str(repo)) == "repo"

    def test_project_from_remote_ssh_protocol(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        repo = tmp_path / "x"
        repo.mkdir()
        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(
                0, "ssh://git@host/path/to/repo.git\n"
            )
            assert _resolve_project(str(repo)) == "repo"

    def test_project_fallback_to_toplevel(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        repo = tmp_path / "my-project"
        repo.mkdir()
        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.side_effect = [
                _mock_run_result(1),  # remote get-url fails
                _mock_run_result(0, str(repo) + "\n"),  # rev-parse succeeds
            ]
            assert _resolve_project(str(repo)) == "my-project"
            assert mock_run.call_count == 2

    def test_project_fallback_to_cwd(self, tmp_path):
        from mindojo_mcp.extract_learnings import _resolve_project

        cwd = tmp_path / "fallback-repo"
        cwd.mkdir()
        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.side_effect = [
                _mock_run_result(1),  # remote fails
                _mock_run_result(1),  # rev-parse fails
            ]
            assert _resolve_project(str(cwd)) == "fallback-repo"

    def test_global_scope_for_home_dir(self):
        from mindojo_mcp.extract_learnings import _resolve_project

        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(1)
            assert _resolve_project(str(Path.home())) is None

    def test_global_scope_for_tmp(self):
        from mindojo_mcp.extract_learnings import _resolve_project

        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(1)
            assert _resolve_project("/tmp") is None

    def test_global_scope_for_root(self):
        from mindojo_mcp.extract_learnings import _resolve_project

        with patch("mindojo_mcp.extract_learnings.subprocess.run") as mock_run:
            mock_run.return_value = _mock_run_result(1)
            assert _resolve_project("/") is None


# ---------------------------------------------------------------------------
# _run (SubagentStop)
# ---------------------------------------------------------------------------


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
            await _run({"last_assistant_message": "x" * 69})
            mock_add.assert_not_called()

    @pytest.mark.asyncio
    async def test_calls_full_pipeline(self):
        from mindojo_mcp.extract_learnings import _run

        message = "A" * 400
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
            mock_add.assert_called_once()
            kwargs = mock_add.call_args.kwargs
            assert kwargs["content"] == message
            assert kwargs["user_id"] == "project:myrepo"
            assert kwargs["metadata"] == {"type": "lesson", "source": "auto-agent-stop"}
            assert kwargs["extraction_prompt"] is not None  # hook uses strict prompt

    @pytest.mark.asyncio
    async def test_global_scope_when_no_project(self):
        from mindojo_mcp.extract_learnings import _run

        message = "B" * 400
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
            ) as mock_add,
            patch("mindojo_mcp.extract_learnings._resolve_project", return_value=None),
        ):
            await _run({"last_assistant_message": message, "cwd": "/home/user"})
            assert mock_add.call_args.kwargs["user_id"] == "global"


# ---------------------------------------------------------------------------
# _run_precompact (PreCompact)
# ---------------------------------------------------------------------------


def _make_jsonl_line(type_: str, role: str = "", text: str = "") -> str:
    """Build a JSONL line matching Claude Code transcript format."""
    obj: dict = {"type": type_}
    if role == "assistant" and text:
        obj["message"] = {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
        }
    elif role == "user":
        obj["message"] = {"role": "user", "content": [{"type": "text", "text": text}]}
    return json.dumps(obj)


class TestRunPrecompact:
    @pytest.mark.asyncio
    async def test_precompact_extracts_last_assistant(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run_precompact

        transcript = tmp_path / "session.jsonl"
        lines = [
            _make_jsonl_line("user", role="user", text="Hello"),
            _make_jsonl_line(
                "assistant", role="assistant", text="First response " + "A" * 400
            ),
            _make_jsonl_line("user", role="user", text="More input"),
            _make_jsonl_line(
                "assistant",
                role="assistant",
                text="Final conclusions with learnings " + "B" * 400,
            ),
        ]
        transcript.write_text("\n".join(lines) + "\n")

        payload = {
            "hook_event_name": "PreCompact",
            "transcript_path": str(transcript),
            "cwd": "/home/user/myrepo",
        }
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
            ) as mock_add,
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="myrepo",
            ),
        ):
            await _run_precompact(payload)
            mock_add.assert_called_once()
            content = mock_add.call_args.kwargs["content"]
            assert content.startswith("Final conclusions")
            assert mock_add.call_args.kwargs["metadata"]["source"] == "auto-pre-compact"

    @pytest.mark.asyncio
    async def test_precompact_skips_short_transcript(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run_precompact

        transcript = tmp_path / "session.jsonl"
        lines = [
            _make_jsonl_line("assistant", role="assistant", text="Short"),
        ]
        transcript.write_text("\n".join(lines) + "\n")

        payload = {
            "hook_event_name": "PreCompact",
            "transcript_path": str(transcript),
            "cwd": "/tmp/x",
        }
        with patch(
            "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
        ) as mock_add:
            await _run_precompact(payload)
            mock_add.assert_not_called()

    @pytest.mark.asyncio
    async def test_precompact_missing_file(self):
        from mindojo_mcp.extract_learnings import _run_precompact

        payload = {
            "hook_event_name": "PreCompact",
            "transcript_path": "/nonexistent/path/session.jsonl",
            "cwd": "/tmp/x",
        }
        with patch(
            "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
        ) as mock_add:
            await _run_precompact(payload)
            mock_add.assert_not_called()

    @pytest.mark.asyncio
    async def test_precompact_skips_tool_only_messages(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run_precompact

        transcript = tmp_path / "session.jsonl"
        tool_msg = {
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_123", "name": "Read"}],
            },
        }
        text_msg = _make_jsonl_line(
            "assistant", role="assistant", text="Good analysis " + "X" * 400
        )
        lines = [json.dumps(tool_msg), text_msg]
        transcript.write_text("\n".join(lines) + "\n")

        payload = {
            "hook_event_name": "PreCompact",
            "transcript_path": str(transcript),
            "cwd": "/home/user/proj",
        }
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory", new_callable=AsyncMock
            ) as mock_add,
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="proj",
            ),
        ):
            await _run_precompact(payload)
            mock_add.assert_called_once()
            content = mock_add.call_args.kwargs["content"]
            assert content.startswith("Good analysis")


# ---------------------------------------------------------------------------
# main() dispatch
# ---------------------------------------------------------------------------


class TestMain:
    def test_main_dispatches_precompact(self, tmp_path):
        from mindojo_mcp.extract_learnings import main

        transcript = tmp_path / "session.jsonl"
        transcript.write_text("")
        payload = json.dumps(
            {
                "hook_event_name": "PreCompact",
                "transcript_path": str(transcript),
                "cwd": "/tmp/x",
            }
        )
        with (
            patch("sys.stdin") as mock_stdin,
            patch(
                "mindojo_mcp.extract_learnings._run_precompact",
                new_callable=AsyncMock,
            ) as mock_precompact,
            patch(
                "mindojo_mcp.extract_learnings._run",
                new_callable=AsyncMock,
            ) as mock_run,
        ):
            mock_stdin.read.return_value = payload
            main()
            mock_precompact.assert_called_once()
            mock_run.assert_not_called()

    def test_main_dispatches_subagent_stop(self):
        from mindojo_mcp.extract_learnings import main

        payload = json.dumps(
            {
                "hook_event_name": "SubagentStop",
                "last_assistant_message": "C" * 400,
                "cwd": "/tmp/x",
            }
        )
        with (
            patch("sys.stdin") as mock_stdin,
            patch(
                "mindojo_mcp.extract_learnings._run_precompact",
                new_callable=AsyncMock,
            ) as mock_precompact,
            patch(
                "mindojo_mcp.extract_learnings._run",
                new_callable=AsyncMock,
            ) as mock_run,
        ):
            mock_stdin.read.return_value = payload
            main()
            mock_run.assert_called_once()
            mock_precompact.assert_not_called()

    def test_main_unknown_hook_does_nothing(self):
        from mindojo_mcp.extract_learnings import main

        payload = json.dumps({"hook_event_name": "UnknownHook", "cwd": "/tmp/x"})
        with (
            patch("sys.stdin") as mock_stdin,
            patch(
                "mindojo_mcp.extract_learnings._run_precompact",
                new_callable=AsyncMock,
            ) as mock_precompact,
            patch(
                "mindojo_mcp.extract_learnings._run",
                new_callable=AsyncMock,
            ) as mock_run,
        ):
            mock_stdin.read.return_value = payload
            main()
            mock_run.assert_not_called()
            mock_precompact.assert_not_called()

    def test_survives_pipeline_failure(self):
        from mindojo_mcp.extract_learnings import main

        payload = json.dumps(
            {
                "hook_event_name": "SubagentStop",
                "last_assistant_message": "C" * 400,
                "cwd": "/tmp/x",
            }
        )
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


# ---------------------------------------------------------------------------
# _log_hook_data
# ---------------------------------------------------------------------------


class TestLogHookData:
    def test_writes_correct_jsonl(self, tmp_path):
        from mindojo_mcp.extract_learnings import _log_hook_data

        log_file = tmp_path / _HOOK_DATA_LOG
        _log_hook_data(
            log_path=log_file,
            hook="SubagentStop",
            project="myrepo",
            user_id="project:myrepo",
            content="some agent output text here",
            facts=["fact one", "fact two"],
            decision="kept",
        )
        lines = log_file.read_text().strip().splitlines()
        assert len(lines) == 1
        entry = json.loads(lines[0])
        assert entry["hook"] == "SubagentStop"
        assert entry["project"] == "myrepo"
        assert entry["user_id"] == "project:myrepo"
        assert entry["content"] == "some agent output text here"
        assert entry["content_length"] == len("some agent output text here")
        assert entry["facts"] == ["fact one", "fact two"]
        assert entry["decision"] == "kept"
        assert entry["prompt_file"] == "fact-extraction-hook.md"
        assert "ts" in entry

    def test_appends_multiple_entries(self, tmp_path):
        from mindojo_mcp.extract_learnings import _log_hook_data

        log_file = tmp_path / _HOOK_DATA_LOG
        _log_hook_data(
            log_path=log_file,
            hook="SubagentStop",
            project="repo1",
            user_id="project:repo1",
            content="text1",
            facts=[],
            decision="rejected",
        )
        _log_hook_data(
            log_path=log_file,
            hook="PreCompact",
            project="repo2",
            user_id="project:repo2",
            content="text2",
            facts=None,
            decision="error",
        )
        lines = log_file.read_text().strip().splitlines()
        assert len(lines) == 2
        assert json.loads(lines[0])["decision"] == "rejected"
        assert json.loads(lines[1])["decision"] == "error"

    def test_never_crashes_on_write_error(self, tmp_path):
        from mindojo_mcp.extract_learnings import _log_hook_data

        # Point to a non-existent directory so open() fails
        bad_path = tmp_path / "nonexistent" / "subdir" / _HOOK_DATA_LOG
        _log_hook_data(
            log_path=bad_path,
            hook="SubagentStop",
            project="x",
            user_id="project:x",
            content="text",
            facts=[],
            decision="rejected",
        )
        # Should not raise — just logs the error


# ---------------------------------------------------------------------------
# _run with hook data logging
# ---------------------------------------------------------------------------


class TestRunLogging:
    @pytest.mark.asyncio
    async def test_logs_kept_entry(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run

        log_file = tmp_path / _HOOK_DATA_LOG
        message = "A" * 400
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
                return_value=["fact1", "fact2"],
            ),
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="myrepo",
            ),
            patch(
                "mindojo_mcp.extract_learnings._HOOK_DATA_LOG_PATH",
                log_file,
            ),
        ):
            await _run({"last_assistant_message": message, "cwd": "/home/user/myrepo"})

        lines = log_file.read_text().strip().splitlines()
        assert len(lines) == 1
        entry = json.loads(lines[0])
        assert entry["decision"] == "kept"
        assert entry["facts"] == ["fact1", "fact2"]
        assert entry["hook"] == "SubagentStop"

    @pytest.mark.asyncio
    async def test_logs_rejected_entry(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run

        log_file = tmp_path / _HOOK_DATA_LOG
        message = "B" * 400
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
                return_value=[],
            ),
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="myrepo",
            ),
            patch(
                "mindojo_mcp.extract_learnings._HOOK_DATA_LOG_PATH",
                log_file,
            ),
        ):
            await _run({"last_assistant_message": message, "cwd": "/home/user/myrepo"})

        entry = json.loads(log_file.read_text().strip())
        assert entry["decision"] == "rejected"
        assert entry["facts"] == []

    @pytest.mark.asyncio
    async def test_logs_error_entry(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run

        log_file = tmp_path / _HOOK_DATA_LOG
        message = "C" * 400
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
                return_value=None,
            ),
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="myrepo",
            ),
            patch(
                "mindojo_mcp.extract_learnings._HOOK_DATA_LOG_PATH",
                log_file,
            ),
        ):
            await _run({"last_assistant_message": message, "cwd": "/home/user/myrepo"})

        entry = json.loads(log_file.read_text().strip())
        assert entry["decision"] == "error"

    @pytest.mark.asyncio
    async def test_logs_skipped_short_entry(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run

        log_file = tmp_path / _HOOK_DATA_LOG
        short_msg = "x" * 50
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
            ) as mock_add,
            patch(
                "mindojo_mcp.extract_learnings._HOOK_DATA_LOG_PATH",
                log_file,
            ),
        ):
            await _run(
                {"last_assistant_message": short_msg, "cwd": "/home/user/myrepo"}
            )
            mock_add.assert_not_called()

        entry = json.loads(log_file.read_text().strip())
        assert entry["decision"] == "skipped_short"
        assert entry["content"] == short_msg

    @pytest.mark.asyncio
    async def test_precompact_logs_kept_entry(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run_precompact

        log_file = tmp_path / _HOOK_DATA_LOG
        transcript = tmp_path / "session.jsonl"
        text = "Final conclusions " + "X" * 400
        lines = [_make_jsonl_line("assistant", role="assistant", text=text)]
        transcript.write_text("\n".join(lines) + "\n")

        payload = {
            "hook_event_name": "PreCompact",
            "transcript_path": str(transcript),
            "cwd": "/home/user/myrepo",
        }
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
                return_value=["lesson1"],
            ),
            patch(
                "mindojo_mcp.extract_learnings._resolve_project",
                return_value="myrepo",
            ),
            patch(
                "mindojo_mcp.extract_learnings._HOOK_DATA_LOG_PATH",
                log_file,
            ),
        ):
            await _run_precompact(payload)

        entry = json.loads(log_file.read_text().strip())
        assert entry["decision"] == "kept"
        assert entry["hook"] == "PreCompact"
        assert entry["facts"] == ["lesson1"]

    @pytest.mark.asyncio
    async def test_precompact_logs_skipped_short(self, tmp_path):
        from mindojo_mcp.extract_learnings import _run_precompact

        log_file = tmp_path / _HOOK_DATA_LOG
        transcript = tmp_path / "session.jsonl"
        lines = [_make_jsonl_line("assistant", role="assistant", text="Short")]
        transcript.write_text("\n".join(lines) + "\n")

        payload = {
            "hook_event_name": "PreCompact",
            "transcript_path": str(transcript),
            "cwd": "/tmp/x",
        }
        with (
            patch(
                "mindojo_mcp.extract_learnings.do_add_memory",
                new_callable=AsyncMock,
            ) as mock_add,
            patch(
                "mindojo_mcp.extract_learnings._HOOK_DATA_LOG_PATH",
                log_file,
            ),
        ):
            await _run_precompact(payload)
            mock_add.assert_not_called()

        entry = json.loads(log_file.read_text().strip())
        assert entry["decision"] == "skipped_short"
        assert entry["hook"] == "PreCompact"
