"""Write the fixtures of the core's tests without Python again (Issue #225).

Each file under `native/core/tests/data/` that `tests/support/core_fixtures.py` registers
(`FIXTURES`) is written by its builder: worlds the Python side built and the values the native
path gives on them. `tests/integration/test_core_fixtures.py` checks that each file is what its
builder makes today; run this after a change that moves them.

Run from the repository root with the native extension built:
    SHELTERSIM_LLM_MODE=offline python benchmarks/core_fixtures.py [name ...]
With no name, every registered fixture is written.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main(names: list[str]) -> None:
    sys.path.insert(0, str(ROOT / "tests"))
    from support.core_fixtures import FIXTURES, write

    unknown = sorted(set(names) - set(FIXTURES))
    if unknown:
        raise SystemExit(f"no such fixture: {', '.join(unknown)} (known: {', '.join(FIXTURES)})")
    for name in names or list(FIXTURES):
        path = write(name)
        print(f"wrote {path.relative_to(ROOT)} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main(sys.argv[1:])
