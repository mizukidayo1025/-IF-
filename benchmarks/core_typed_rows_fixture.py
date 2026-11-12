"""Write the fixture of the core's typed rows (Issue #225, F1).

`native/core/tests/data/typed_rows.json` holds the typed namespaces of `model_state` and worlds
of `tests/support/core_typed_rows.py` that together have each of them.
`native/core/tests/typed_rows.rs` reads each namespace into its row without Python and writes it
back to the same bytes; `tests/integration/test_core_typed_rows.py` checks that the fixture is
still that module's document.

Run from the repository root with the native extension built:
    SHELTERSIM_LLM_MODE=offline python benchmarks/core_typed_rows_fixture.py
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "native" / "core" / "tests" / "data" / "typed_rows.json"


def main() -> None:
    sys.path.insert(0, str(ROOT / "tests"))
    from support.core_typed_rows import typed_rows_text

    FIXTURE.write_text(typed_rows_text(), encoding="utf-8")
    print(f"wrote {FIXTURE.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
