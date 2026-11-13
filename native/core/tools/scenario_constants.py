"""Write the constants of the core's Scenario builder (Issue #299).

    SHELTERSIM_LLM_MODE=offline python native/core/tools/scenario_constants.py

`native/core/src/scenario/constants.json` gets what `tests/support/core_scenario_builder.py`'s
`builder_constants` reads from Python: the model configs every Scenario-screen request gets (as
the validated Scenario holds them), the default policy IDs and the source IDs Python's modules
define. `tests/unit/experiments/test_initial_scenario.py` fails when the file is stale.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TARGET = ROOT / "native" / "core" / "src" / "scenario" / "constants.json"


def main() -> None:
    sys.path.insert(0, str(ROOT / "tests"))
    from support.core_scenario_builder import constants_text

    TARGET.write_text(constants_text(), encoding="utf-8")
    print(f"wrote {TARGET.relative_to(ROOT)} ({TARGET.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
