"""Turn on the refusal census (refusal_census.py) in any process, before the simulator starts.

Put this directory first on PYTHONPATH and run anything:

    SHELTERSIM_REFUSAL_CENSUS=DIR PYTHONPATH=benchmarks/rule_shadow:src:native/pylib \
        python benchmarks/gate_all_paths.py --output OUT --expect fixtures/gate-all-paths.json

Each process, the worker processes of a Project among them, writes what it counted to DIR at
exit. Without SHELTERSIM_REFUSAL_CENSUS this file does nothing.

The shadows that once lived here compared each kernel with its Python rule at every call. Those
rules went with lane L10, and the shadows with them (docs/rust-port/lanes/ has their history).
"""

from __future__ import annotations

import importlib
import os

CENSUS_OUTPUT = os.environ.get("SHELTERSIM_REFUSAL_CENSUS")

if CENSUS_OUTPUT:
    importlib.import_module("refusal_census").install(CENSUS_OUTPUT)
