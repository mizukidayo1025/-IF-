"""Write the fixture of the core's tick test without Python (Issue #152, P10-1b).

`native/core/tests/data/tick_core.json` holds the small world of `tests/support/core_tick.py`
at minute 0, what the Run read of its controller, and the digests the native path gives after
`TICKS` ticks through `TickEngine` (offline). `native/core/tests/tick_core.rs` builds the World
from it and must reach the same digests; `tests/integration/test_core_tick.py` checks that the
fixture is still this module's world and the native path's result.

Run from the repository root with the native extension built:
    SHELTERSIM_LLM_MODE=offline python benchmarks/core_tick_fixture.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "native" / "core" / "tests" / "data" / "tick_core.json"


def main() -> None:
    sys.path.insert(0, str(ROOT / "tests"))
    from support.core_tick import core_tick_run, fixture_document, native_result

    world, controller, _ = core_tick_run()
    document = fixture_document(world, controller)
    document["expected"] = native_result()
    FIXTURE.write_text(
        json.dumps(document, sort_keys=True, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
    )
    print(f"wrote {FIXTURE.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
