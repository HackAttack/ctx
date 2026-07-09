#!/usr/bin/env python3
"""Export ctx message events into a Claude Code JSONL fixture for Obliscence.

This is a benchmark adapter, not a compatibility claim: it preserves session
IDs, roles, timestamps, and message text from a ctx work.sqlite database while
wrapping them in the minimal Claude Code envelope that Obliscence indexes.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sqlite3
import sys
import time
from datetime import datetime, timezone


def iso_from_ms(ms: int) -> str:
    return datetime.fromtimestamp(ms / 1000, tz=timezone.utc).isoformat().replace(
        "+00:00", "Z"
    )


def encode_project(path: str) -> str:
    # Claude Code encodes cwd-ish paths as dash-separated project dirs.
    cleaned = path.strip("/") or "ctx-codex-export"
    return "-" + cleaned.replace("/", "-")


def extract_text(payload_json: str) -> tuple[str, str | None]:
    try:
        payload = json.loads(payload_json)
    except json.JSONDecodeError:
        return "", None
    body = payload.get("body") or {}
    text = body.get("text") or ""
    phase = body.get("phase")
    if body.get("truncated") and text:
        text += "\n[ctx export note: original preview was truncated]"
    return text, phase


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", required=True, help="ctx work.sqlite path")
    parser.add_argument(
        "--out-home",
        required=True,
        help="temporary HOME where .claude/projects will be created",
    )
    parser.add_argument(
        "--project-path",
        default="/ctx/codex-export",
        help="cwd/project path to put in the Claude JSONL envelope",
    )
    parser.add_argument(
        "--limit-sessions",
        type=int,
        default=0,
        help="optional cap for quick smoke tests",
    )
    args = parser.parse_args()

    db_path = pathlib.Path(args.db)
    if not db_path.exists():
        raise SystemExit(f"missing ctx db: {db_path}")

    out_home = pathlib.Path(args.out_home)
    projects_dir = out_home / ".claude" / "projects" / encode_project(args.project_path)
    projects_dir.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row

    session_clause = ""
    params: list[object] = []
    if args.limit_sessions > 0:
        session_clause = (
            "AND e.session_id IN ("
            "SELECT id FROM sessions ORDER BY started_at_ms LIMIT ?"
            ")"
        )
        params.append(args.limit_sessions)

    query = f"""
        SELECT
            e.id AS event_id,
            e.session_id,
            COALESCE(s.external_session_id, e.session_id) AS external_session_id,
            e.role,
            e.occurred_at_ms,
            e.payload_json
        FROM events e
        JOIN sessions s ON s.id = e.session_id
        WHERE e.event_type = 'message'
          AND e.role IN ('user', 'assistant')
          {session_clause}
        ORDER BY e.session_id, e.occurred_at_ms, e.seq
    """

    started = time.perf_counter()
    current_session = None
    current_file = None
    parent_uuid = None
    session_count = 0
    message_count = 0
    skipped_empty = 0

    try:
        for row in conn.execute(query, params):
            session_id = row["external_session_id"]
            if session_id != current_session:
                if current_file is not None:
                    current_file.close()
                current_session = session_id
                parent_uuid = None
                session_count += 1
                current_file = open(projects_dir / f"{session_id}.jsonl", "w", encoding="utf-8")

            text, phase = extract_text(row["payload_json"])
            if not text.strip():
                skipped_empty += 1
                continue

            event_uuid = row["event_id"]
            role = row["role"]
            message: dict[str, object]
            if role == "assistant":
                message = {
                    "role": "assistant",
                    "model": "codex-export",
                    "content": [{"type": "text", "text": text}],
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                }
            else:
                message = {
                    "role": "user",
                    "content": [{"type": "text", "text": text}],
                }

            envelope = {
                "type": role,
                "sessionId": session_id,
                "uuid": event_uuid,
                "parentUuid": parent_uuid,
                "timestamp": iso_from_ms(row["occurred_at_ms"]),
                "cwd": args.project_path,
                "gitBranch": "",
                "message": message,
            }
            if phase == "summary":
                envelope["isCompactSummary"] = True

            assert current_file is not None
            current_file.write(json.dumps(envelope, ensure_ascii=False, separators=(",", ":")))
            current_file.write("\n")
            parent_uuid = event_uuid
            message_count += 1
    finally:
        if current_file is not None:
            current_file.close()
        conn.close()

    elapsed = time.perf_counter() - started
    print(
        json.dumps(
            {
                "projects_dir": str(projects_dir),
                "sessions": session_count,
                "messages": message_count,
                "skipped_empty": skipped_empty,
                "seconds": elapsed,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
