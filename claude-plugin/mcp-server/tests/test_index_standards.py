"""Tests for scripts/index-standards.py pure functions."""

from __future__ import annotations

import importlib
import sys
from pathlib import Path

# Add scripts to path so we can import the module
_scripts_dir = Path(__file__).resolve().parent.parent.parent.parent / "scripts"
_mcp_src = Path(__file__).resolve().parent.parent / "src"
sys.path.insert(0, str(_scripts_dir))
sys.path.insert(0, str(_mcp_src))

index_standards = importlib.import_module("index-standards")


# -- chunk_markdown ----------------------------------------------------------


class TestChunkMarkdown:
    def test_empty_input(self):
        chunks = index_standards.chunk_markdown("")
        assert len(chunks) == 1
        assert chunks[0]["body"] == ""
        assert chunks[0]["heading"] == ""
        assert chunks[0]["level"] == 0

    def test_no_headings(self):
        text = "Just some plain text."
        chunks = index_standards.chunk_markdown(text)
        assert len(chunks) == 1
        assert chunks[0]["body"] == text
        assert chunks[0]["level"] == 0

    def test_single_h2(self):
        text = "## Section One\n\nBody text here."
        chunks = index_standards.chunk_markdown(text)
        assert len(chunks) == 1
        assert chunks[0]["heading"] == "Section One"
        assert chunks[0]["level"] == 2
        assert "Body text here." in chunks[0]["body"]

    def test_h2_h3_hierarchy(self):
        text = "## Chapter\n\nIntro.\n\n### Sub\n\nDetail."
        chunks = index_standards.chunk_markdown(text)
        assert len(chunks) == 2
        assert chunks[0]["heading"] == "Chapter"
        assert chunks[0]["parent_heading"] == ""
        assert chunks[1]["heading"] == "Sub"
        assert chunks[1]["parent_heading"] == "Chapter"
        assert chunks[1]["level"] == 3

    def test_preamble_before_heading(self):
        text = "Preamble text.\n\n## Heading\n\nBody."
        chunks = index_standards.chunk_markdown(text)
        assert len(chunks) == 2
        assert chunks[0]["heading"] == ""
        assert chunks[0]["body"] == "Preamble text."
        assert chunks[1]["heading"] == "Heading"

    def test_heading_with_no_body(self):
        text = "## Empty\n\n## Next\n\nSome body."
        chunks = index_standards.chunk_markdown(text)
        assert len(chunks) == 2
        assert chunks[0]["heading"] == "Empty"
        assert chunks[0]["body"] == ""
        assert chunks[1]["heading"] == "Next"

    def test_multiple_h3_under_h2(self):
        text = "## Parent\n\nP body.\n\n### A\n\nA body.\n\n### B\n\nB body."
        chunks = index_standards.chunk_markdown(text)
        assert len(chunks) == 3
        assert chunks[1]["parent_heading"] == "Parent"
        assert chunks[2]["parent_heading"] == "Parent"

    def test_h2_resets_parent(self):
        text = "## Ch1\n\n### Sub1\n\nBody.\n\n## Ch2\n\n### Sub2\n\nBody."
        chunks = index_standards.chunk_markdown(text)
        assert chunks[1]["parent_heading"] == "Ch1"
        assert chunks[3]["parent_heading"] == "Ch2"


# -- fallback_metadata -------------------------------------------------------


class TestFallbackMetadata:
    def test_fields_present(self):
        meta = index_standards.fallback_metadata("Title", "Chapter")
        assert meta["section_id"] == ""
        assert meta["section_title"] == "Title"
        assert meta["chapter"] == "Chapter"
        assert meta["ref_ids"] == []
        assert meta["code_patterns"] == ""

    def test_empty_heading(self):
        meta = index_standards.fallback_metadata("", "")
        assert meta["section_title"] == ""
        assert meta["chapter"] == ""


# -- _validate_metadata ------------------------------------------------------


class TestValidateMetadata:
    def test_valid_passthrough(self):
        meta = {
            "section_id": "V5.1.2",
            "section_title": "Input Validation",
            "chapter": "V5",
            "ref_ids": ["ASVS-5.1.2", "CWE-20"],
            "code_patterns": "validate(input)",
        }
        result = index_standards._validate_metadata(dict(meta))
        assert result == meta

    def test_invalid_ref_ids_filtered(self):
        meta = {
            "section_id": "ok",
            "section_title": "T",
            "chapter": "",
            "ref_ids": ["good-1", "bad one with spaces", 42, "also/good:2"],
            "code_patterns": "",
        }
        result = index_standards._validate_metadata(meta)
        assert result["ref_ids"] == ["good-1", "also/good:2"]

    def test_ref_ids_non_list(self):
        meta = {"ref_ids": "not-a-list"}
        result = index_standards._validate_metadata(meta)
        assert result["ref_ids"] == []

    def test_section_id_invalid(self):
        meta = {"section_id": "has spaces!"}
        result = index_standards._validate_metadata(meta)
        assert result["section_id"] == ""

    def test_section_id_non_string(self):
        meta = {"section_id": 123}
        result = index_standards._validate_metadata(meta)
        assert result["section_id"] == ""

    def test_section_title_truncated(self):
        meta = {"section_title": "A" * 300}
        result = index_standards._validate_metadata(meta)
        assert len(result["section_title"]) == 200

    def test_section_title_non_string(self):
        meta = {"section_title": 42}
        result = index_standards._validate_metadata(meta)
        assert result["section_title"] == "42"

    def test_chapter_truncated(self):
        meta = {"chapter": "B" * 250}
        result = index_standards._validate_metadata(meta)
        assert len(result["chapter"]) == 200

    def test_chapter_stripped(self):
        meta = {"chapter": "  spaced  "}
        result = index_standards._validate_metadata(meta)
        assert result["chapter"] == "spaced"

    def test_code_patterns_non_string(self):
        meta = {"code_patterns": ["a", "b"]}
        result = index_standards._validate_metadata(meta)
        assert isinstance(result["code_patterns"], str)

    def test_missing_fields_get_defaults(self):
        meta = {}
        result = index_standards._validate_metadata(meta)
        assert result["ref_ids"] == []
        assert result["section_id"] == ""
        assert result["section_title"] == ""
        assert result["chapter"] == ""
        assert result["code_patterns"] == ""


# -- build_chunk_text --------------------------------------------------------


class TestBuildChunkText:
    def test_h2_with_body(self):
        chunk = {"heading": "Title", "level": 2, "body": "Content."}
        result = index_standards.build_chunk_text(chunk)
        assert result == "## Title\n\nContent."

    def test_h3_with_body(self):
        chunk = {"heading": "Sub", "level": 3, "body": "Detail."}
        result = index_standards.build_chunk_text(chunk)
        assert result == "### Sub\n\nDetail."

    def test_no_heading(self):
        chunk = {"heading": "", "level": 0, "body": "Just text."}
        result = index_standards.build_chunk_text(chunk)
        assert result == "Just text."

    def test_heading_no_body(self):
        chunk = {"heading": "Empty", "level": 2, "body": ""}
        result = index_standards.build_chunk_text(chunk)
        assert result == "## Empty"
