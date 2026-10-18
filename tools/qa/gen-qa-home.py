"""Generates crates/gui/tests/fixtures/qa-home, the synthetic QA sandbox corpus (DESIGN section 6).

    python tools/qa/gen-qa-home.py            (from the repository root; the output is committed)

The corpus is invented: three projects (one with a git worktree), tool calls, thinking, code, tables,
an empty transcript, a transcript with one malformed line, and a Pi and an OMP session copied from
core's fixtures. tools/qa/new-sandbox.ps1 copies it into a fresh sandbox home and adds the
.qa-sandbox marker, so the folder itself never carries the marker.
"""

import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "crates" / "gui" / "tests" / "fixtures" / "qa-home"
CORE_FIXTURES = ROOT / "crates" / "core" / "tests" / "fixtures"

# Synthetic Windows-style working directories (never touched: they only name projects).
PROJ = "C:\\sbx\\proj"
API = "C:\\sbx\\api"
WORKTREE = "C:\\sbx\\proj\\.worktrees\\feature"


def encode(cwd: str) -> str:
    return "".join(c if c.isalnum() or c == "-" else "-" for c in cwd)


def uuid(n: int) -> str:
    return f"00000000-0000-4000-8000-{n:012x}"


def stamp(day: int, minute: int) -> str:
    return f"2026-09-{1 + day % 28:02d}T10:{minute % 60:02d}:00Z"


def user(session, cwd, ts, content):
    return {"type": "user", "sessionId": session, "cwd": cwd, "timestamp": ts,
            "message": {"role": "user", "content": content}}


def assistant(session, cwd, ts, blocks, model="claude-opus-4-5"):
    return {"type": "assistant", "sessionId": session, "cwd": cwd, "timestamp": ts,
            "message": {"role": "assistant", "model": model, "content": blocks,
                        "usage": {"input_tokens": 120, "output_tokens": 340}}}


def text(s):
    return {"type": "text", "text": s}


def tool_use(i, name, **inp):
    return {"type": "tool_use", "id": f"toolu_{i}", "name": name, "input": inp}


def tool_result(i, content):
    return {"type": "tool_result", "tool_use_id": f"toolu_{i}", "content": content}


def conversation(n, cwd, title, day, turns):
    """turns: list of (question, [assistant blocks], optional tool result content for the first tool_use)."""
    sid = uuid(n)
    lines = [{"type": "summary", "summary": title, "leafUuid": f"leaf-{sid}"}]
    minute = 0
    for q, blocks in turns:
        lines.append(user(sid, cwd, stamp(day, minute), q))
        minute += 1
        tool_ids = [b["id"] for b in blocks if b["type"] == "tool_use"]
        lines.append(assistant(sid, cwd, stamp(day, minute), blocks))
        minute += 1
        for tid in tool_ids:
            n_id = tid.split("_", 1)[1]
            lines.append(user(sid, cwd, stamp(day, minute), [tool_result(n_id, f"result of {tid}:\nline one\nline two\nline three")]))
            minute += 1
    return sid, lines


def write_lines(path: Path, lines, malformed_at=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    out = [json.dumps(l, separators=(",", ":")) for l in lines]
    if malformed_at is not None:
        out.insert(malformed_at, "{this is not json")
    path.write_text("\n".join(out) + ("\n" if out else ""), encoding="utf-8", newline="\n")


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    projects = OUT / ".claude" / "projects"

    def add(n, cwd, title, day, turns, malformed_at=None):
        sid, lines = conversation(n, cwd, title, day, turns)
        write_lines(projects / encode(cwd) / f"{sid}.jsonl", lines, malformed_at)

    code = "```rust\nfn refresh(token: &Token) -> Result<Token, AuthError> {\n    client.post(\"/refresh\").json(token).send()\n}\n```"
    table = "| Endpoint | Limit |\n|---|---|\n| /login | 10/min |\n| /refresh | 60/min |"

    add(1, PROJ, "Fix login bug", 1, [
        ("The login form rejects valid passwords. Can you find out why?", [
            text("I will look at the login handler first."),
            tool_use(11, "Read", file_path="C:\\sbx\\proj\\src\\login.rs")]),
        ("Thanks, now fix it.", [
            {"type": "thinking", "thinking": "The comparison trims only one side.", "signature": "sig"},
            text("The password was compared without trimming the stored hash. Fixed in `login.rs`.")]),
    ])
    add(2, PROJ, "Token refresh retry", 3, [
        ("Add a retry to the token refresh.", [text("Here is the change:\n\n" + code)]),
        ("Does it back off?", [text("Yes: it waits 200 ms, 400 ms, 800 ms before giving up.")]),
    ])
    add(3, API, "Add API pagination", 5, [
        ("Paginate the list endpoint.", [
            text("Searching for the handler."),
            tool_use(31, "Grep", pattern="fn list_items", path="C:\\sbx\\api")]),
        ("Use cursors, not offsets.", [text("Switched to cursor pagination; the page size is capped at 100.")]),
    ])
    add(4, API, "Rate limit errors", 7, [
        ("Which endpoints are rate limited?", [text("These:\n\n" + table)]),
    ])
    add(5, PROJ, "Deploy pipeline", 9, [
        ("Why did the deploy stage fail?", [text("The artifact name contained a space.")]),
        ("Fix the script.", [
            tool_use(51, "Edit", file_path="C:\\sbx\\proj\\deploy.ps1", old_string="$name", new_string="\"$name\"")]),
    ])
    add(6, WORKTREE, "Worktree cleanup", 11, [
        ("Remove stale worktrees.", [text("Removed two stale worktrees.")]),
    ])
    # An empty transcript (no messages) and one with a malformed line.
    write_lines(projects / encode(PROJ) / f"{uuid(90)}.jsonl", [])
    sid, lines = conversation(91, API, "Malformed line", 13, [("One good question.", [text("One good answer.")])])
    write_lines(projects / encode(API) / f"{sid}.jsonl", lines, malformed_at=1)

    # Pi and OMP sessions (core's fixtures).
    pi = OUT / ".pi" / "agent" / "sessions" / "--tmp-pi-project--"
    pi.mkdir(parents=True)
    shutil.copyfile(CORE_FIXTURES / "pi" / "v3-branched.jsonl",
                    pi / "2024-03-01T00-00-00-000Z_01912345-6789-7abc-8def-0123456789ab.jsonl")
    omp = OUT / ".omp" / "agent" / "sessions" / "--tmp-omp-project--"
    omp.mkdir(parents=True)
    shutil.copyfile(CORE_FIXTURES / "omp" / "v3.jsonl", omp / "2026-01-02T03-04-05-000Z_omp_session_custom_id.jsonl")

    (OUT / "README.txt").write_text(
        "Synthetic QA sandbox corpus (generated by tools/qa/gen-qa-home.py).\n"
        "Copy it with tools/qa/new-sandbox.ps1; the copy gets the .qa-sandbox marker.\n",
        encoding="utf-8", newline="\n")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    sys.exit(main())
