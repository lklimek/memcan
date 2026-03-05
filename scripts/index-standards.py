#!/usr/bin/env python3
"""Index markdown standards documents into Qdrant for semantic search.

Splits a markdown file on ## and ### headings, extracts metadata via LLM,
embeds each chunk, and upserts into the standards collection.

Must be run from claude-plugin/mcp-server/ so mindojo_mcp imports resolve.

Usage:
    cd claude-plugin/mcp-server
    uv run python ../../scripts/index-standards.py doc.md \
        --standard-id ASVS-5.0 --standard-type security
"""

from __future__ import annotations

import argparse
import json
import logging
import re
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

from ollama import Client as OllamaClient
from qdrant_client.models import (
    Filter,
    FieldCondition,
    MatchValue,
    PayloadSchemaType,
)

from mindojo_mcp.config import EXTRACTION_MODEL, STANDARDS_COLLECTION, settings
from mindojo_mcp.prompts import _load
from mindojo_mcp.qdrant_utils import PointStruct, embed, ensure_collection, get_qdrant

logger = logging.getLogger(__name__)

HEADING_RE = re.compile(r"^(#{2,3})\s+(.+)", re.MULTILINE)
VALID_TYPES = ("security", "coding", "cve", "guideline")

KEYWORD_INDEXES = (
    "standard_id",
    "standard_type",
    "version",
    "ref_ids",
    "tech_stack",
    "lang",
)


def _setup_logging(verbose: bool, log_file: str | None) -> None:
    level = logging.DEBUG if verbose else logging.INFO
    handlers: list[logging.Handler] = [logging.StreamHandler()]
    if log_file:
        handlers.append(logging.FileHandler(log_file))
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
        handlers=handlers,
    )


def chunk_markdown(text: str) -> list[dict[str, str]]:
    """Split markdown on ## and ### headings, tracking hierarchy.

    Returns list of dicts with keys: heading, parent_heading, level, body.
    """
    matches = list(HEADING_RE.finditer(text))
    if not matches:
        return [{"heading": "", "parent_heading": "", "level": 0, "body": text.strip()}]

    chunks: list[dict[str, str]] = []

    preamble = text[: matches[0].start()].strip()
    if preamble:
        chunks.append(
            {"heading": "", "parent_heading": "", "level": 0, "body": preamble}
        )

    current_h2 = ""
    for i, m in enumerate(matches):
        level = len(m.group(1))
        heading = m.group(2).strip()
        start = m.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(text)
        body = text[start:end].strip()

        if level == 2:
            current_h2 = heading
            parent = ""
        else:
            parent = current_h2

        chunks.append(
            {
                "heading": heading,
                "parent_heading": parent,
                "level": level,
                "body": body,
            }
        )

    return chunks


def extract_metadata(
    chunk_text: str,
    model: str,
    timeout: int,
) -> dict | None:
    """Call Ollama to extract metadata from a chunk. Returns parsed dict or None."""
    prompt = _load("metadata-extraction.md", chunk_text=chunk_text)
    client = OllamaClient(host=settings.ollama_url)
    resp = client.chat(
        model=model,
        messages=[{"role": "user", "content": prompt}],
        options={"num_predict": 512, "temperature": 0.0},
        format="json",
    )
    raw = resp.message.content.strip()
    return json.loads(raw)


def fallback_metadata(heading: str, parent_heading: str) -> dict:
    """Build minimal metadata from heading info when LLM extraction fails."""
    return {
        "section_id": "",
        "section_title": heading,
        "chapter": parent_heading,
        "ref_ids": [],
        "code_patterns": "",
    }


def create_keyword_indexes(collection: str) -> None:
    """Create keyword payload indexes on the collection."""
    qd = get_qdrant()
    for field in KEYWORD_INDEXES:
        try:
            qd.create_payload_index(
                collection_name=collection,
                field_name=field,
                field_schema=PayloadSchemaType.KEYWORD,
            )
            logger.debug("Created index on %s.%s", collection, field)
        except Exception:
            logger.debug("Index on %s.%s may already exist", collection, field)


def build_chunk_text(chunk: dict[str, str]) -> str:
    """Reconstruct readable text from a chunk for embedding and LLM."""
    parts: list[str] = []
    if chunk["heading"]:
        prefix = "##" if chunk["level"] == 2 else "###"
        parts.append(f"{prefix} {chunk['heading']}")
    if chunk["body"]:
        parts.append(chunk["body"])
    return "\n\n".join(parts)


def process_file(args: argparse.Namespace) -> int:
    """Index a single markdown file. Returns exit code."""
    md_path = Path(args.file)
    if not md_path.is_file():
        logger.error("File not found: %s", md_path)
        return 1

    text = md_path.read_text(encoding="utf-8")
    chunks = chunk_markdown(text)
    logger.info("Parsed %d chunks from %s", len(chunks), md_path.name)

    ensure_collection(STANDARDS_COLLECTION)
    create_keyword_indexes(STANDARDS_COLLECTION)

    start_index = args.retry_from or 0
    if start_index:
        logger.info("Resuming from chunk %d", start_index)

    errors: list[dict] = []
    indexed = 0
    now = datetime.now(timezone.utc).isoformat()

    for chunk_index, chunk in enumerate(chunks):
        if chunk_index < start_index:
            continue

        chunk_text = build_chunk_text(chunk)
        if not chunk_text.strip():
            logger.debug("Skipping empty chunk %d", chunk_index)
            continue

        try:
            meta = None
            for attempt in range(2):
                try:
                    meta = extract_metadata(chunk_text, args.model, args.llm_timeout)
                    if meta is not None:
                        break
                except Exception as exc:
                    if attempt == 0:
                        logger.warning(
                            "LLM extraction failed for chunk %d (retrying): %s",
                            chunk_index,
                            exc,
                        )
                    else:
                        logger.warning(
                            "LLM extraction failed for chunk %d (using fallback): %s",
                            chunk_index,
                            exc,
                        )

            if meta is None:
                meta = fallback_metadata(chunk["heading"], chunk["parent_heading"])

            vector = embed([chunk_text])[0]

            section_id = meta.get("section_id", "")
            point_id = str(
                uuid.uuid5(
                    uuid.NAMESPACE_URL,
                    f"{args.standard_id}:{section_id}:{chunk_index}",
                )
            )

            payload = {
                "data": chunk_text,
                "standard_id": args.standard_id,
                "standard_type": args.standard_type,
                "version": args.version,
                "ref_ids": meta.get("ref_ids", []),
                "section_id": section_id,
                "section_title": meta.get("section_title", ""),
                "chapter": meta.get("chapter", ""),
                "tech_stack": args.tech_stack,
                "lang": args.lang,
                "url": args.url,
                "source_path": str(md_path),
                "code_patterns": meta.get("code_patterns", ""),
                "indexed_at": now,
            }

            point = PointStruct(id=point_id, vector=vector, payload=payload)
            get_qdrant().upsert(collection_name=STANDARDS_COLLECTION, points=[point])
            indexed += 1
            logger.info(
                "Indexed chunk %d/%d: %s",
                chunk_index,
                len(chunks) - 1,
                payload["section_title"] or "(untitled)",
            )

        except Exception as exc:
            logger.error("Failed chunk %d: %s", chunk_index, exc)
            errors.append(
                {
                    "chunk_index": chunk_index,
                    "heading": chunk["heading"],
                    "error": str(exc),
                }
            )

    logger.info("Indexed %d chunks, %d errors", indexed, len(errors))

    if errors:
        error_path = Path(args.error_file)
        error_path.write_text(json.dumps(errors, indent=2), encoding="utf-8")
        logger.warning("Errors written to %s", error_path)

    return 1 if errors else 0


def drop_standard(args: argparse.Namespace) -> int:
    """Drop all points for a given standard_id."""
    if not args.standard_id:
        logger.error("--standard-id is required with --drop")
        return 1

    ensure_collection(STANDARDS_COLLECTION)
    qd = get_qdrant()
    filt = Filter(
        must=[
            FieldCondition(key="standard_id", match=MatchValue(value=args.standard_id))
        ]
    )
    count = qd.count(
        collection_name=STANDARDS_COLLECTION, count_filter=filt, exact=True
    ).count
    if count == 0:
        logger.info("No points found for standard_id=%s", args.standard_id)
        return 0

    qd.delete(collection_name=STANDARDS_COLLECTION, points_selector=filt)
    logger.info("Deleted %d points for standard_id=%s", count, args.standard_id)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Index markdown standards documents into Qdrant"
    )
    parser.add_argument("file", nargs="?", help="Markdown file to index")
    parser.add_argument("--standard-id", required=True, help="Standard identifier")
    parser.add_argument(
        "--standard-type",
        choices=VALID_TYPES,
        help="Type of standard (required unless --drop)",
    )
    parser.add_argument("--version", default="", help="Standard version")
    parser.add_argument("--lang", default="en", help="Language code (default: en)")
    parser.add_argument("--tech-stack", default="", help="Technology stack")
    parser.add_argument("--url", default="", help="Source URL")
    parser.add_argument(
        "--model", default=EXTRACTION_MODEL, help="Ollama model for extraction"
    )
    parser.add_argument(
        "--drop", action="store_true", help="Drop all points for --standard-id"
    )
    parser.add_argument("--verbose", action="store_true", help="Debug logging")
    parser.add_argument("--log-file", help="Write logs to file")
    parser.add_argument(
        "--error-file",
        default="index-standards-errors.json",
        help="JSON file for per-chunk errors",
    )
    parser.add_argument(
        "--llm-timeout", type=int, default=30, help="LLM call timeout seconds"
    )
    parser.add_argument(
        "--retry-from", type=int, default=0, help="Resume from chunk index"
    )

    args = parser.parse_args()
    _setup_logging(args.verbose, args.log_file)

    if args.drop:
        return drop_standard(args)

    if not args.file:
        parser.error("file is required unless --drop is specified")
    if not args.standard_type:
        parser.error("--standard-type is required unless --drop is specified")

    return process_file(args)


if __name__ == "__main__":
    sys.exit(main())
