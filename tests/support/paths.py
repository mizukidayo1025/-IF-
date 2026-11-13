"""Paths of the files under fixtures/ that tests read.

Test files may import these constants directly, as they may the stateless synthetic
data of other support modules (docs/testing.md); anything with state or cleanup in
tests/support/ reaches them as a fixture.
"""

from __future__ import annotations

from pathlib import Path

FIXTURES = Path(__file__).parents[2] / "fixtures"
SCENARIOS = FIXTURES / "scenarios"
SCENARIO_100_AGENTS = FIXTURES / "scenario_100_agents.json"
SMALL_REPLAY = SCENARIOS / "h00_small_replay.json"
BASELINE_100_7D = SCENARIOS / "baseline_100_7d.json"
FAILURE_DAY3 = SCENARIOS / "failure_day3.json"
RESEARCH_OPERATIONAL_100_7D = SCENARIOS / "research_operational_100_7d.json"
REGRESSION_CONTRACT = FIXTURES / "regression-contract.json"
GATE_ALL_PATHS = FIXTURES / "gate-all-paths.json"
SOURCE_MANIFEST = FIXTURES / "source-manifest.json"
AGENTS_INITIAL = FIXTURES / "agents_initial.json"
PROFILES_SMALL = FIXTURES / "profiles_small.json"
GYM_LAYOUT = FIXTURES / "layouts" / "gym_synthetic.json"
DEMO_REPLAY = FIXTURES / "demo_replay"
# A Run saved before Checkpoints recorded the whole-model version (engine version 1).
SAVED_RUN_ENGINE_V1 = FIXTURES / "saved_runs" / "engine-v1-small"
BENCHMARKS = FIXTURES.parent / "benchmarks"
# The fixtures of the core's tests without Python (native/core/tests/, #152, #225), which
# support/core_fixtures.py writes.
CORE_TEST_DATA = FIXTURES.parent / "native" / "core" / "tests" / "data"
# The table the core's minute-0 builder reads (native/core/src/initial/, #249), which
# support/core_initial.py writes.
INITIAL_TABLE = FIXTURES.parent / "native" / "core" / "src" / "initial" / "table.json"
