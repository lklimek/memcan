#!/usr/bin/env python3
"""Migrate memories from old Qdrant collection to new one.

Re-embeds text with the current embedding model. Preserves IDs, payloads,
and metadata. Idempotent — skips points that already exist in target.

Usage:
    uv run python scripts/migrate-collection.py [--dry-run]
"""

from __future__ import annotations

import argparse
import sys

from qdrant_client import QdrantClient
from qdrant_client.models import Distance, PointStruct, VectorParams

# These must match config.py constants
OLD_COLLECTION = "mindojo"
NEW_COLLECTION = "mindojo-memories"
EMBED_MODEL = "qwen3-embedding:4b"
EMBED_DIMS = 2560


def main():
    parser = argparse.ArgumentParser(description="Migrate Qdrant collection")
    parser.add_argument(
        "--dry-run", action="store_true", help="Print plan without writing"
    )
    parser.add_argument("--qdrant-url", default="http://localhost:6333")
    parser.add_argument(
        "--ollama-url", default=None, help="Ollama URL (reads .env if omitted)"
    )
    args = parser.parse_args()

    # Resolve Ollama URL from .env if not provided
    ollama_url = args.ollama_url
    if not ollama_url:
        from mindojo_mcp.config import settings

        ollama_url = settings.ollama_url

    qdrant = QdrantClient(url=args.qdrant_url)

    # Check old collection exists
    collections = {c.name for c in qdrant.get_collections().collections}
    if OLD_COLLECTION not in collections:
        print(f"✗ Source collection '{OLD_COLLECTION}' not found. Nothing to migrate.")
        sys.exit(0)

    # Create new collection if needed
    if NEW_COLLECTION not in collections:
        print(f"Creating collection '{NEW_COLLECTION}' ({EMBED_DIMS}d, cosine)…")
        if not args.dry_run:
            qdrant.create_collection(
                collection_name=NEW_COLLECTION,
                vectors_config=VectorParams(size=EMBED_DIMS, distance=Distance.COSINE),
            )
    else:
        print(f"Collection '{NEW_COLLECTION}' already exists.")

    # Read all points from old collection
    all_points = []
    offset = None
    while True:
        result = qdrant.scroll(
            collection_name=OLD_COLLECTION,
            limit=100,
            offset=offset,
            with_payload=True,
            with_vectors=False,
        )
        points, next_offset = result
        all_points.extend(points)
        if next_offset is None:
            break
        offset = next_offset

    print(f"Found {len(all_points)} points in '{OLD_COLLECTION}'.")

    if not all_points:
        print("Nothing to migrate.")
        return

    if args.dry_run:
        for p in all_points:
            data = p.payload.get("data", "?")[:80]
            uid = p.payload.get("user_id", "?")
            print(f"  [{uid}] {data}")
        print(
            f"\nDry run: would migrate {len(all_points)} points. Re-run without --dry-run."
        )
        return

    # Embed all texts with new model
    from ollama import Client

    ollama = Client(host=ollama_url)
    texts = [p.payload.get("data", "") for p in all_points]

    print(f"Embedding {len(texts)} texts with {EMBED_MODEL}…")
    response = ollama.embed(model=EMBED_MODEL, input=texts)
    embeddings = response["embeddings"]
    assert len(embeddings) == len(all_points), "Embedding count mismatch"
    assert len(embeddings[0]) == EMBED_DIMS, (
        f"Expected {EMBED_DIMS}d, got {len(embeddings[0])}d"
    )

    # Upsert into new collection (idempotent)
    new_points = [
        PointStruct(
            id=str(p.id),
            vector=emb,
            payload=p.payload,
        )
        for p, emb in zip(all_points, embeddings)
    ]

    BATCH = 50
    for i in range(0, len(new_points), BATCH):
        batch = new_points[i : i + BATCH]
        qdrant.upsert(collection_name=NEW_COLLECTION, points=batch)
        print(f"  Upserted {i + len(batch)}/{len(new_points)}")

    # Verify
    new_count = qdrant.count(collection_name=NEW_COLLECTION, exact=True).count
    print(f"\n✓ Migration complete: {new_count} points in '{NEW_COLLECTION}'.")
    print(
        f"  Old collection '{OLD_COLLECTION}' left intact. Delete manually when ready:"
    )
    print(f"  curl -X DELETE {args.qdrant_url}/collections/{OLD_COLLECTION}")


if __name__ == "__main__":
    # Add mcp-server src to path for config import
    sys.path.insert(
        0,
        str(
            __import__("pathlib").Path(__file__).resolve().parent.parent
            / "claude-plugin"
            / "mcp-server"
            / "src"
        ),
    )
    main()
