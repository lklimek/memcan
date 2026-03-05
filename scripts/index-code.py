#!/usr/bin/env python3
"""Index source code into Qdrant using tree-sitter for symbol extraction.

Parses supported languages (Rust, Python, Go, TypeScript) with tree-sitter,
extracts top-level symbols, embeds them, and upserts into the mindojo-code
Qdrant collection. Falls back to 100-line chunks when parsing fails.

Usage (run from claude-plugin/mcp-server/):
    uv run python ../../scripts/index-code.py /path/to/project --project myproj --tech-stack rust
    uv run python ../../scripts/index-code.py /path/to/project --project myproj --drop
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import logging
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

from qdrant_client.models import (
    FieldCondition,
    Filter,
    MatchValue,
    PayloadSchemaType,
)
from tree_sitter_language_pack import get_parser

from mindojo_mcp.config import CODE_COLLECTION, EMBED_DIMS
from mindojo_mcp.qdrant_utils import (
    PointStruct,
    drop_by_filter,
    embed,
    ensure_collection,
    get_qdrant,
)

logger = logging.getLogger(__name__)

SKIP_DIRS = {".git", "node_modules", "target", ".venv", "__pycache__", "dist", "build"}

LANG_EXTENSIONS: dict[str, list[str]] = {
    "rust": [".rs"],
    "python": [".py"],
    "go": [".go"],
    "typescript": [".ts", ".tsx"],
}

LANG_NODES: dict[str, set[str]] = {
    "rust": {
        "function_item",
        "impl_item",
        "struct_item",
        "enum_item",
        "trait_item",
        "mod_item",
    },
    "python": {"function_definition", "class_definition"},
    "go": {"function_declaration", "method_declaration", "type_declaration"},
    "typescript": {
        "function_declaration",
        "class_declaration",
        "interface_declaration",
        "type_alias_declaration",
    },
}

UUID_NAMESPACE = uuid.UUID("a3e1f8c0-7b2d-4e5a-9f1c-6d8b0e3a5c7f")

CHUNK_LINES = 100


def _ext_to_lang(ext: str) -> str | None:
    for lang, exts in LANG_EXTENSIONS.items():
        if ext in exts:
            return lang
    return None


def _all_extensions() -> set[str]:
    return {ext for exts in LANG_EXTENSIONS.values() for ext in exts}


def _should_skip(path: Path) -> bool:
    return any(part in SKIP_DIRS for part in path.parts)


def _get_symbol_name(node) -> str:
    name_node = node.child_by_field_name("name")
    if name_node:
        return name_node.text.decode()
    # impl_item: use the 'type' field
    type_node = node.child_by_field_name("type")
    if type_node:
        return type_node.text.decode()
    # Go type_declaration: name is inside child type_spec
    for child in node.children:
        if child.type == "type_spec":
            spec_name = child.child_by_field_name("name")
            if spec_name:
                return spec_name.text.decode()
    return "<anonymous>"


def _git_short_hash(project_dir: Path) -> str:
    try:
        return (
            subprocess.check_output(
                ["git", "rev-parse", "--short", "HEAD"],
                cwd=project_dir,
                stderr=subprocess.DEVNULL,
            )
            .decode()
            .strip()
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def _content_hash(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def _point_id(project: str, file_path: str, symbol_name: str, start_line: int) -> str:
    key = f"{project}:{file_path}:{symbol_name}:{start_line}"
    return str(uuid.uuid5(UUID_NAMESPACE, key))


def _context_line(file_path: str, lang: str, tech_stack: str) -> str:
    return f"# file: {file_path} | lang: {lang} | stack: {tech_stack}"


def _extract_symbols(source: bytes, lang: str, file_path: str) -> list[dict]:
    """Parse source with tree-sitter and extract top-level symbols.

    Returns list of dicts with keys: text, symbol_name, start_line, end_line, chunk_type.
    """
    target_nodes = LANG_NODES.get(lang)
    if not target_nodes:
        return []

    try:
        parser = get_parser(lang)
    except Exception:
        logger.warning("No tree-sitter parser for %s", lang)
        return []

    try:
        tree = parser.parse(source)
    except Exception:
        logger.warning("tree-sitter parse failed for %s", file_path)
        return []

    symbols = []
    for node in tree.root_node.children:
        if node.type not in target_nodes:
            continue
        name = _get_symbol_name(node)
        text = node.text.decode(errors="replace")
        symbols.append(
            {
                "text": text,
                "symbol_name": name,
                "start_line": node.start_point.row + 1,
                "end_line": node.end_point.row + 1,
                "chunk_type": node.type,
            }
        )
    return symbols


def _chunk_fallback(source: str, file_path: str) -> list[dict]:
    """Split source into fixed-size line chunks when tree-sitter fails."""
    lines = source.splitlines(keepends=True)
    chunks = []
    for i in range(0, len(lines), CHUNK_LINES):
        chunk_lines = lines[i : i + CHUNK_LINES]
        text = "".join(chunk_lines)
        if not text.strip():
            continue
        chunks.append(
            {
                "text": text,
                "symbol_name": f"chunk_{i // CHUNK_LINES}",
                "start_line": i + 1,
                "end_line": i + len(chunk_lines),
                "chunk_type": "chunk",
            }
        )
    return chunks


def _collect_files(project_dir: Path) -> list[Path]:
    valid_exts = _all_extensions()
    files = []
    for p in sorted(project_dir.rglob("*")):
        if not p.is_file():
            continue
        if _should_skip(p.relative_to(project_dir)):
            continue
        if p.suffix in valid_exts:
            files.append(p)
    return files


def _ensure_keyword_indexes(collection: str) -> None:
    qd = get_qdrant()
    for field in ("project", "tech_stack", "file_path", "chunk_type"):
        try:
            qd.create_payload_index(
                collection_name=collection,
                field_name=field,
                field_schema=PayloadSchemaType.KEYWORD,
            )
        except Exception:
            pass  # index already exists


def _get_existing_hashes(collection: str, project: str) -> dict[str, dict]:
    """Return {point_id: {content_hash, file_path}} for all project points."""
    qd = get_qdrant()
    existing: dict[str, dict] = {}
    offset = None
    filt = Filter(must=[FieldCondition(key="project", match=MatchValue(value=project))])
    while True:
        points, next_offset = qd.scroll(
            collection_name=collection,
            scroll_filter=filt,
            limit=100,
            offset=offset,
            with_payload=True,
            with_vectors=False,
        )
        for p in points:
            existing[str(p.id)] = {
                "content_hash": p.payload.get("content_hash", ""),
                "file_path": p.payload.get("file_path", ""),
            }
        if next_offset is None:
            break
        offset = next_offset
    return existing


def _delete_removed_files(
    collection: str, project: str, existing_files: set[str], indexed_files: set[str]
) -> int:
    """Delete points for files no longer present in the project."""
    removed = indexed_files - existing_files
    deleted = 0
    for fp in removed:
        filt = Filter(
            must=[
                FieldCondition(key="project", match=MatchValue(value=project)),
                FieldCondition(key="file_path", match=MatchValue(value=fp)),
            ]
        )
        deleted += drop_by_filter(collection, filt)
    return deleted


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Index source code into Qdrant with tree-sitter symbol extraction"
    )
    parser.add_argument(
        "project_dir", type=Path, help="Root directory of the project to index"
    )
    parser.add_argument(
        "--project", required=True, help="Project name for payload tagging"
    )
    parser.add_argument(
        "--tech-stack", default=None, help="Tech stack label (required unless --drop)"
    )
    parser.add_argument(
        "--drop", action="store_true", help="Drop all indexed data for this project"
    )
    parser.add_argument("--verbose", action="store_true", help="Enable debug logging")
    parser.add_argument(
        "--log-file", type=Path, default=None, help="Write logs to file"
    )
    parser.add_argument(
        "--error-file", type=Path, default=None, help="Write errors to file"
    )
    parser.add_argument(
        "--max-file-size",
        type=int,
        default=1_048_576,
        help="Skip files larger than this many bytes (default: 1 MB)",
    )
    args = parser.parse_args()

    # Logging setup
    log_level = logging.DEBUG if args.verbose else logging.INFO
    handlers: list[logging.Handler] = [logging.StreamHandler()]
    if args.log_file:
        handlers.append(logging.FileHandler(args.log_file))
    logging.basicConfig(
        level=log_level, format="%(levelname)s %(message)s", handlers=handlers
    )

    collection = CODE_COLLECTION

    if args.drop:
        ensure_collection(collection, EMBED_DIMS)
        filt = Filter(
            must=[FieldCondition(key="project", match=MatchValue(value=args.project))]
        )
        deleted = drop_by_filter(collection, filt)
        logger.info("Dropped %d points for project '%s'", deleted, args.project)
        return

    if not args.tech_stack:
        parser.error("--tech-stack is required unless --drop is specified")

    project_dir = args.project_dir.resolve()
    if not project_dir.is_dir():
        logger.error("Project directory does not exist: %s", project_dir)
        sys.exit(1)

    ensure_collection(collection, EMBED_DIMS)
    _ensure_keyword_indexes(collection)

    git_hash = _git_short_hash(project_dir)
    now = datetime.now(timezone.utc).isoformat()

    # Get existing indexed data for incremental re-indexing
    existing = _get_existing_hashes(collection, args.project)
    indexed_file_paths = {v["file_path"] for v in existing.values()}

    files = _collect_files(project_dir)
    logger.info("Found %d source files in %s", len(files), project_dir)

    current_file_paths: set[str] = set()
    total_upserted = 0
    total_skipped = 0
    total_errors = 0
    batch: list[PointStruct] = []
    BATCH_SIZE = 20

    def flush_batch() -> int:
        nonlocal batch
        if not batch:
            return 0
        texts = [p.payload["data"] for p in batch]
        try:
            vectors = embed(texts)
        except Exception as exc:
            logger.error("Embedding failed for batch of %d: %s", len(batch), exc)
            batch = []
            return 0
        for point, vec in zip(batch, vectors):
            point.vector = vec
        qd = get_qdrant()
        qd.upsert(collection_name=collection, points=batch)
        count = len(batch)
        batch = []
        return count

    with contextlib.ExitStack() as stack:
        error_fh = None
        if args.error_file:
            error_fh = stack.enter_context(open(args.error_file, "w"))

        for file_path in files:
            rel_path = str(file_path.relative_to(project_dir))
            current_file_paths.add(rel_path)
            lang = _ext_to_lang(file_path.suffix)

            if file_path.is_symlink():
                logger.warning("Skipping symlink: %s", rel_path)
                continue

            if file_path.stat().st_size > args.max_file_size:
                logger.warning(
                    "Skipping %s: size %d exceeds limit %d",
                    rel_path,
                    file_path.stat().st_size,
                    args.max_file_size,
                )
                continue

            try:
                source_bytes = file_path.read_bytes()
                source_text = source_bytes.decode(errors="replace")
            except Exception as exc:
                msg = f"Failed to read {rel_path}: {exc}"
                logger.error(msg)
                if error_fh:
                    error_fh.write(msg + "\n")
                total_errors += 1
                continue

            # Extract symbols or fall back to chunks
            symbols = []
            if lang:
                symbols = _extract_symbols(source_bytes, lang, rel_path)
            if not symbols:
                symbols = _chunk_fallback(source_text, rel_path)
            if not symbols:
                continue

            effective_lang = lang or "unknown"

            for sym in symbols:
                ctx = _context_line(rel_path, effective_lang, args.tech_stack)
                data = f"{ctx}\n{sym['text']}"
                chash = _content_hash(data)
                pid = _point_id(
                    args.project, rel_path, sym["symbol_name"], sym["start_line"]
                )

                # Skip if content unchanged
                if pid in existing and existing[pid]["content_hash"] == chash:
                    total_skipped += 1
                    continue

                payload = {
                    "data": data,
                    "project": args.project,
                    "file_path": rel_path,
                    "tech_stack": args.tech_stack,
                    "chunk_type": sym["chunk_type"],
                    "symbol_name": sym["symbol_name"],
                    "start_line": sym["start_line"],
                    "end_line": sym["end_line"],
                    "content_hash": chash,
                    "git_hash": git_hash,
                    "indexed_at": now,
                }

                batch.append(
                    PointStruct(id=pid, vector=[0.0] * EMBED_DIMS, payload=payload)
                )

                if len(batch) >= BATCH_SIZE:
                    total_upserted += flush_batch()
                    logger.info("  Upserted %d symbols so far...", total_upserted)

    total_upserted += flush_batch()

    # Delete points for removed files
    deleted = _delete_removed_files(
        collection, args.project, current_file_paths, indexed_file_paths
    )

    logger.info(
        "Done: %d upserted, %d unchanged, %d errors, %d deleted (removed files)",
        total_upserted,
        total_skipped,
        total_errors,
        deleted,
    )


if __name__ == "__main__":
    sys.path.insert(
        0,
        str(
            Path(__file__).resolve().parent.parent
            / "claude-plugin"
            / "mcp-server"
            / "src"
        ),
    )
    main()
