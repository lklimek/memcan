"""Unit tests for search_standards and search_code MCP tools."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, patch

import pytest

from mindojo_mcp.config import CODE_COLLECTION, STANDARDS_COLLECTION


@pytest.fixture()
def _mock_asearch():
    """Patch asearch_collection in server module to return sample results."""
    sample = [{"score": 0.95, "title": "test doc", "content": "hello"}]
    with patch(
        "mindojo_mcp.server.asearch_collection",
        new_callable=AsyncMock,
        return_value=sample,
    ) as mock:
        yield mock


class TestSearchStandards:
    """search_standards tool tests."""

    @pytest.mark.asyncio
    async def test_returns_json_from_mock(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        result = await search_standards(query="buffer overflow")
        parsed = json.loads(result)
        assert isinstance(parsed, list)
        assert parsed[0]["score"] == 0.95

    @pytest.mark.asyncio
    async def test_passes_standards_collection(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss")
        call_kwargs = _mock_asearch.call_args
        assert call_kwargs.kwargs["collection"] == STANDARDS_COLLECTION

    @pytest.mark.asyncio
    async def test_builds_filter_dict(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(
            query="xss",
            standard_type="security",
            standard_id="owasp-asvs",
            tech_stack="python",
            lang="en",
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] == "owasp-asvs"
        assert filters["tech_stack"] == "python"
        assert filters["lang"] == "en"

    @pytest.mark.asyncio
    async def test_case_insensitive_filters(self, _mock_asearch):
        """Uppercase filter values are normalized to lowercase."""
        from mindojo_mcp.server import search_standards

        await search_standards(
            query="xss", standard_type="SECURITY", standard_id="OWASP-ASVS"
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] == "owasp-asvs"

    @pytest.mark.asyncio
    async def test_type_alias_owasp_resolves_to_security(self, _mock_asearch):
        """standard_type='owasp' resolves to type=security."""
        from mindojo_mcp.server import search_standards

        await search_standards(query="password storage", standard_type="owasp")
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] is None  # broad OWASP search

    @pytest.mark.asyncio
    async def test_type_alias_asvs_resolves_to_security_and_id(self, _mock_asearch):
        """standard_type='asvs' resolves to type=security + id=owasp-asvs."""
        from mindojo_mcp.server import search_standards

        await search_standards(query="session management", standard_type="ASVS")
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] == "owasp-asvs"

    @pytest.mark.asyncio
    async def test_type_alias_does_not_override_explicit_id(self, _mock_asearch):
        """When both type alias and explicit standard_id given, explicit id wins."""
        from mindojo_mcp.server import search_standards

        await search_standards(
            query="xss", standard_type="owasp", standard_id="owasp-cheatsheets"
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] == "owasp-cheatsheets"

    @pytest.mark.asyncio
    async def test_id_alias_resolves(self, _mock_asearch):
        """standard_id='asvs' resolves to 'owasp-asvs'."""
        from mindojo_mcp.server import search_standards

        await search_standards(query="auth", standard_id="asvs")
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_id"] == "owasp-asvs"

    @pytest.mark.asyncio
    async def test_ref_id_wraps_to_ref_ids_list(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", ref_id="CWE-79")
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["ref_ids"] == ["CWE-79"]

    @pytest.mark.asyncio
    async def test_ref_id_none_passes_none(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", ref_id=None)
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["ref_ids"] is None

    @pytest.mark.asyncio
    async def test_limit_clamped_low(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", limit=0)
        assert _mock_asearch.call_args.kwargs["limit"] == 1

    @pytest.mark.asyncio
    async def test_limit_clamped_high(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", limit=999)
        assert _mock_asearch.call_args.kwargs["limit"] == 100


class TestSearchCode:
    """search_code tool tests."""

    @pytest.mark.asyncio
    async def test_returns_json_from_mock(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        result = await search_code(query="error handling")
        parsed = json.loads(result)
        assert isinstance(parsed, list)
        assert parsed[0]["score"] == 0.95

    @pytest.mark.asyncio
    async def test_passes_code_collection(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(query="retry logic")
        assert _mock_asearch.call_args.kwargs["collection"] == CODE_COLLECTION

    @pytest.mark.asyncio
    async def test_builds_filter_dict(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(
            query="retry",
            project="myrepo",
            tech_stack="rust",
            file_path="src/main.rs",
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["project"] == "myrepo"
        assert filters["tech_stack"] == "rust"
        assert filters["file_path"] == "src/main.rs"

    @pytest.mark.asyncio
    async def test_limit_clamped_low(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(query="test", limit=-5)
        assert _mock_asearch.call_args.kwargs["limit"] == 1

    @pytest.mark.asyncio
    async def test_limit_clamped_high(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(query="test", limit=200)
        assert _mock_asearch.call_args.kwargs["limit"] == 100
