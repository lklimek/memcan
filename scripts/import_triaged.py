#!/usr/bin/env python3
"""Import triaged memory candidates into Qdrant.

Reads a triage-annotated review-report.json (produced by triage-findings),
filters for findings with action == "fix", and stores them via embed + upsert.

Usage:
    python3 import_triaged.py report.json [--dry-run]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

# Import config from sibling MCP server package
sys.path.insert(
    0,
    str(
        Path(__file__).resolve().parent.parent / "claude-plugin" / "mcp-server" / "src"
    ),
)
from mindojo_mcp.config import QDRANT_COLLECTION  # noqa: E402
from mindojo_mcp.qdrant_utils import embed, ensure_collection, get_qdrant  # noqa: E402

from qdrant_client.models import PointStruct  # noqa: E402


def load_report(path: Path) -> dict:
    """Load and validate the report JSON."""
    data = json.loads(path.read_text())

    if "triage" not in data:
        print("Error: Report has no triage decisions. Run triage-findings first.")
        sys.exit(1)

    return data


def extract_project_from_recommendation(recommendation: str) -> str | None:
    """Parse 'Import as project:<name> memory' → '<name>'."""
    match = __import__("re").search(r"project:(\S+)", recommendation)
    return match.group(1) if match else None


def build_findings_map(report: dict) -> dict[str, dict]:
    """Build finding_id → finding dict from all sections."""
    findings_map: dict[str, dict] = {}
    for section in report.get("findings", []):
        for finding in section.get("findings", []):
            findings_map[finding["id"]] = finding
    return findings_map


def import_memories(report: dict, *, dry_run: bool = False) -> tuple[int, int]:
    """Import fix-marked findings into Qdrant.

    Returns:
        (imported_count, skipped_count)
    """
    if not dry_run:
        ensure_collection(QDRANT_COLLECTION)

    qd = None if dry_run else get_qdrant()

    findings_map = build_findings_map(report)
    triage_decisions = report["triage"].get("decisions", [])

    imported = 0
    skipped = 0

    for decision in triage_decisions:
        finding_id = decision["finding_id"]
        action = decision["action"]

        if action != "fix":
            skipped += 1
            continue

        finding = findings_map.get(finding_id)
        if not finding:
            print(f"Warning: Finding {finding_id} not found in report, skipping")
            skipped += 1
            continue

        # Determine scope from recommendation
        project = extract_project_from_recommendation(finding.get("recommendation", ""))
        user_id = f"project:{project}" if project else "global"

        # Build memory content — title + description
        content = f"{finding['title']}\n\n{finding['description']}"

        metadata = {
            "source_id": finding_id,
            "tags": finding.get("tags", []),
            "imported_from": finding.get("location", ""),
        }

        if dry_run:
            print(f"  [DRY RUN] Would import {finding_id} → {user_id}")
            print(f"    Title: {finding['title']}")
            print(f"    Content length: {len(content)} chars")
        else:
            vectors = embed([content])
            point_id = str(uuid4())
            payload = {
                "data": content,
                "hash": hashlib.md5(content.encode()).hexdigest(),
                "user_id": user_id,
                "created_at": datetime.now(timezone.utc).isoformat(),
                "updated_at": None,
                **metadata,
            }
            qd.upsert(  # type: ignore[union-attr]
                collection_name=QDRANT_COLLECTION,
                points=[PointStruct(id=point_id, vector=vectors[0], payload=payload)],
            )
            print(f"  Imported {finding_id} → {user_id}: {finding['title']}")

        imported += 1

    return imported, skipped


def main() -> None:
    parser = argparse.ArgumentParser(description="Import triaged memories into Qdrant")
    parser.add_argument("report", help="Path to triaged report.json")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be imported without storing",
    )
    args = parser.parse_args()

    report_path = Path(args.report)
    if not report_path.exists():
        print(f"Error: {report_path} not found")
        sys.exit(1)

    report = load_report(report_path)

    print(f"Processing triaged report: {report_path}")
    print(
        f"Triage by: {report['triage'].get('triaged_by', 'unknown')} "
        f"at {report['triage'].get('triaged_at', 'unknown')}"
    )

    imported, skipped = import_memories(report, dry_run=args.dry_run)

    print(f"\nDone: {imported} imported, {skipped} skipped")


if __name__ == "__main__":
    main()
