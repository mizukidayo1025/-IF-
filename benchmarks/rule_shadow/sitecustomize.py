"""Compare the Rust steps with their Python rules at every call, in any process.

This file has the validator of psychological_distress_v1, movement_shadow.py the state of
movement_v1, world_view_shadow.py the views, in DIR/world_view, and behavior_shadow.py the
proposals, in DIR/behavior. The psychology, social and information steps have no Python rule left
to compare with (lane L10).

Hashes do not see a 1% change in a value that doesn't reach them yet, and a kernel may never run if
it always says Unsupported. Put this directory first on PYTHONPATH and run anything:

    SHELTERSIM_RULE_SHADOW=DIR PYTHONPATH=benchmarks/rule_shadow:src:native/pylib \
        python benchmarks/gate_all_paths.py --output OUT --expect fixtures/gate-all-paths.json
    python benchmarks/rule_shadow/summary.py DIR

Each step runs the Python rules on plain copies first, then the kernel on the world. The two must
be the same text of JSON, in which a float keeps its bits and 1 differs from 1.0, and the row must
write the bytes of the tree. Each process writes what it saw to DIR at exit, so the worker
processes of a Project count too. The kernel's result is what goes on, so the hashes of such a run
must still pass. Without SHELTERSIM_RULE_SHADOW this file does nothing.

docs/rust-port/lanes/p5-template-namespace.md says how a kernel's shadow is made.
A new shadow is a file shadow_<name>.py with prepare(output) beside this one (see shadow_kit.py).
Each runs first, so it can replace a kernel before the module that looks it up is imported.
"""

from __future__ import annotations

import atexit
import importlib
import json
import os
from collections import Counter
from pathlib import Path
from typing import Any

OUTPUT = os.environ.get("SHELTERSIM_RULE_SHADOW")
# Not a shadow: it counts refusals (refusal_census.py), and needs no comparison to run.
CENSUS_OUTPUT = os.environ.get("SHELTERSIM_REFUSAL_CENSUS")
# Not a shadow either: it compares saved outcome texts with the Python rules' (text_check.py).
TEXT_OUTPUT = os.environ.get("SHELTERSIM_TEXT_CHECK")


def _install(output: str) -> None:
    from sheltersim import native
    from sheltersim.core import state_mirror
    from sheltersim.domain.psychological_state import (
        PSYCHOLOGICAL_STATE_KEY,
        validate_psychological_state,
    )

    seen: Counter[str] = Counter()
    reasons: Counter[str] = Counter()
    differences: list[str] = []

    def validator(world: Any) -> bool:
        accepted = rust_validate(world)
        seen["validator_accepted" if accepted else "validator_refused"] += 1
        if accepted:
            # Raises, and fails the run, if the kernel took what the Python rules turn down.
            validate_psychological_state(world.model_state[PSYCHOLOGICAL_STATE_KEY])
        return accepted

    rust_validate = native.state("psychological_distress").validate
    state_mirror.register_rust_validator(PSYCHOLOGICAL_STATE_KEY, validator)

    import movement_shadow

    movement_shadow.install(seen, reasons, differences)

    def report() -> None:
        if seen or reasons or differences:
            target = Path(output) / f"{os.getpid()}.json"
            document = {"seen": seen, "unsupported": reasons, "differences": differences[:20]}
            target.write_text(json.dumps(document), encoding="utf-8")

    atexit.register(report)


if CENSUS_OUTPUT:
    importlib.import_module("refusal_census").install(CENSUS_OUTPUT)

if TEXT_OUTPUT:
    importlib.import_module("text_check").install(
        TEXT_OUTPUT, os.environ.get("SHELTERSIM_TEXT_CHECK_STRICT") == "1"
    )

# Not a shadow either: it compares the kernels' moved errors with the Python rules'
# (error_compare.py).
if os.environ.get("SHELTERSIM_ERROR_COMPARE"):
    importlib.import_module("error_compare").install(os.environ["SHELTERSIM_ERROR_COMPARE"])

if OUTPUT and os.environ.get("SHELTERSIM_DISPATCH_COUNT_ONLY") == "1":
    # A plain run that only says what the dispatch loop's direct calls did (shadow_dispatch.py).
    Path(OUTPUT).mkdir(parents=True, exist_ok=True)
    importlib.import_module("shadow_dispatch").prepare(OUTPUT)
    importlib.import_module("shadow_effects").prepare(OUTPUT)
    importlib.import_module("shadow_handlers_rest").prepare(OUTPUT)
    importlib.import_module("shadow_overlay").prepare(OUTPUT)
    importlib.import_module("shadow_tick").count_ticks(OUTPUT)
elif OUTPUT and os.environ.get("SHELTERSIM_RULE_SHADOW_ONLY"):
    # Only these shadows, by the name after "shadow_", for a lane's own run.
    Path(OUTPUT).mkdir(parents=True, exist_ok=True)
    for name in os.environ["SHELTERSIM_RULE_SHADOW_ONLY"].split(","):
        if name == "update":
            # The validator of psychological_distress_v1 and movement_shadow.py, in this file.
            _install(OUTPUT)
        else:
            importlib.import_module(f"shadow_{name}").prepare(OUTPUT)
elif OUTPUT:
    Path(OUTPUT).mkdir(parents=True, exist_ok=True)
    for shadow in sorted(Path(__file__).parent.glob("shadow_*.py")):
        if shadow.stem != "shadow_kit":
            importlib.import_module(shadow.stem).prepare(OUTPUT)
    _install(OUTPUT)
    import capture_shadow

    capture_shadow.install(OUTPUT)
    import events_shadow

    events_shadow.install(OUTPUT)
    from world_view_shadow import install as _install_world_view

    _install_world_view(OUTPUT)
    from behavior_shadow import install as _install_behavior

    _install_behavior(OUTPUT)
