#!/usr/bin/env python3
"""Generate a review-report.json from memory sources for triage-based import.

Parses LESSONS_LEARNED.md files and agent MEMORY.md files into the Claudius
review-report schema format, enabling selective import via triage-findings UI.

Usage:
    python3 generate_import_report.py [--output report.json]
"""

from __future__ import annotations

import argparse
import json
import re
from datetime import date
from pathlib import Path

# Data sources
LESSONS_LEARNED_SOURCES: list[tuple[str, Path]] = [
    ("penny", Path("/home/ubuntu/git/penny/LESSONS_LEARNED.md")),
]

AGENT_MEMORY_DIR = Path("/home/ubuntu/.claude/agent-memory")


def parse_lessons_learned(project: str, path: Path) -> list[dict]:
    """Parse LESSONS_LEARNED.md into findings.

    Expected format:
        ### Title (LL-NNN)
        - **What went wrong**: ...
        - **Fix**: ...
        - **Prevention**: ...
    """
    if not path.exists():
        return []

    text = path.read_text()
    findings: list[dict] = []

    # Split on ### headings that contain (LL-NNN)
    pattern = r"### (.+?) \((LL-\d{3})\)\s*\n(.*?)(?=\n### |\n## |\Z)"
    matches = re.findall(pattern, text, re.DOTALL)

    for i, (title, ll_id, body) in enumerate(matches, start=1):
        finding_id = f"DOC-{i:03d}"
        description = body.strip()

        findings.append(
            {
                "id": finding_id,
                "severity": "MEDIUM",
                "title": f"[{ll_id}] {title.strip()}",
                "tags": ["lesson", "imported", project],
                "location": f"{path}",
                "description": description,
                "recommendation": f"Import as project:{project} memory",
            }
        )

    return findings


def parse_agent_memory(agent_dir: Path) -> list[dict]:
    """Parse agent MEMORY.md into findings.

    Splits on ## headings, each becomes a finding.
    """
    memory_file = agent_dir / "MEMORY.md"
    if not memory_file.exists():
        return []

    agent_name = agent_dir.name
    text = memory_file.read_text()
    findings: list[dict] = []

    # Split on ## headings
    sections = re.split(r"\n## ", text)
    # First section might be a top-level heading with #
    if sections and sections[0].startswith("# "):
        sections = sections[1:]  # skip the title

    for i, section in enumerate(sections, start=1):
        lines = section.strip().split("\n", 1)
        if not lines:
            continue

        title = lines[0].strip().lstrip("#").strip()
        body = lines[1].strip() if len(lines) > 1 else title

        if not body or len(body) < 10:
            continue

        # Use a high offset to avoid ID collisions with lesson findings
        finding_id = f"DOC-{100 + i:03d}"

        findings.append(
            {
                "id": finding_id,
                "severity": "LOW",
                "title": f"[{agent_name}] {title}",
                "tags": ["agent-memory", "imported", agent_name],
                "location": str(memory_file),
                "description": body,
                "recommendation": "Import as global memory",
            }
        )

    return findings


def build_report(findings_by_source: dict[str, list[dict]]) -> dict:
    """Build a review-report.json conforming to the Claudius schema."""
    all_findings: list[dict] = []
    for findings in findings_by_source.values():
        all_findings.extend(findings)

    # Reassign IDs to avoid collisions
    for i, finding in enumerate(all_findings, start=1):
        finding["id"] = f"DOC-{i:03d}"

    # Build finding sections
    sections: list[dict] = []
    for source_name, findings in findings_by_source.items():
        if not findings:
            continue
        sections.append(
            {
                "title": f"Memories from {source_name}",
                "category": "documentation",
                "findings": findings,
            }
        )

    # Severity counts
    severity_counts = {"CRITICAL": 0, "HIGH": 0, "MEDIUM": 0, "LOW": 0, "INFO": 0}
    for f in all_findings:
        severity_counts[f["severity"]] += 1

    return {
        "schema_version": "1.0.0",
        "metadata": {
            "project": "mindojo-import",
            "date": date.today().isoformat(),
            "report_type": "code_review",
            "reviewers": ["memory-importer"],
        },
        "executive_summary": {
            "overall_assessment": (
                f"Memory import candidates: {len(all_findings)} items "
                f"from {len(sections)} sources ready for triage."
            ),
        },
        "summary_statistics": {
            "total_findings": len(all_findings),
            "severity_counts": severity_counts,
        },
        "findings": sections,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate memory import report")
    parser.add_argument(
        "--output", "-o", default="report.json", help="Output file path"
    )
    args = parser.parse_args()

    findings_by_source: dict[str, list[dict]] = {}

    # Parse lessons learned
    for project, path in LESSONS_LEARNED_SOURCES:
        findings = parse_lessons_learned(project, path)
        if findings:
            findings_by_source[f"lessons-learned/{project}"] = findings

    # Parse agent memories
    if AGENT_MEMORY_DIR.exists():
        for agent_dir in sorted(AGENT_MEMORY_DIR.iterdir()):
            if agent_dir.is_dir():
                findings = parse_agent_memory(agent_dir)
                if findings:
                    findings_by_source[f"agent-memory/{agent_dir.name}"] = findings

    if not findings_by_source:
        print("No memory sources found.")
        return

    report = build_report(findings_by_source)

    output_path = Path(args.output)
    output_path.write_text(json.dumps(report, indent=2) + "\n")
    print(
        f"Generated {output_path} with {report['summary_statistics']['total_findings']} findings"
    )


if __name__ == "__main__":
    main()
