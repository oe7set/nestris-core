# /// script
# requires-python = ">=3.10"
# ///
"""Release text from CHANGELOG.md (same file in every Retroverse repository).

    uv run --script .github/scripts/release_notes.py <version> CHANGELOG.md out.md

Writes the section ``## <version>`` (up to the next ``## ``) to ``out.md``,
without the heading. The apps' updaters show this text before installing.
A version without a section gives an empty file (the workflow then only has
GitHub's generated notes); a missing CHANGELOG.md is an error.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def section(text: str, version: str) -> str:
    version = version.removeprefix("v")
    heading = re.compile(rf"^## \[?v?{re.escape(version)}\]?(\s|$)")
    lines = text.splitlines()
    out: list[str] = []
    inside = False
    for line in lines:
        if line.startswith("## "):
            if inside:
                break
            inside = bool(heading.match(line))
            continue
        if inside:
            out.append(line)
    return "\n".join(out).strip() + "\n" if any(s.strip() for s in out) else ""


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    version, changelog, target = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
    notes = section(changelog.read_text(encoding="utf-8"), version)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(notes, encoding="utf-8")
    print(notes or f"(no section for {version} in {changelog})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
