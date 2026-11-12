"""Settings for every test; the shared setup lives in tests/support/, one module per topic.

The modules are loaded as plugins below, so their fixtures reach every test file
as before. docs/testing.md lists what each module holds.
"""

from __future__ import annotations

import pytest
from support.isolation import _isolate

pytest_plugins = [
    # Writes each run's results under .test-results/ (tests/results_log.py, #136).
    "results_log",
    "support.generated",
    "support.fixture_scenarios",
    "support.demo",
    "support.storage",
    "support.web",
    "support.llm",
    "support.reports",
    "support.world",
    "support.plans",
    "support.public_data",
    # The python_rules mark, which turns the Rust twins of the validators off for a test.
    "support.python_rules",
]


@pytest.fixture(autouse=True)
def _isolated_local_state(
    request: pytest.FixtureRequest,
    tmp_path_factory: pytest.TempPathFactory,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Keep every test away from the developer's Projects, usage ledgers and LLM cache.

    Without this, a CLI test wrote a usage account under the repository's runs/.projects,
    and parallel workers (pytest -n) would share those default locations. Tests that set
    these variables themselves still override them.

    Paid live_api tests keep the real locations on purpose: their spend must count
    against this PC's campaign ledger.
    """
    if request.node.get_closest_marker("live_api") is not None:
        return
    _isolate(monkeypatch, tmp_path_factory.mktemp("local-state"))


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--shard",
        default=None,
        metavar="I/N",
        help="run only part I of N (1 <= I <= N): the selected tests taken in turn (#118)",
    )


def _shard(config: pytest.Config) -> tuple[int, int] | None:
    value = config.getoption("--shard")
    if value is None:
        return None
    part, _, count = value.partition("/")
    try:
        index, total = int(part), int(count)
    except ValueError:
        index = total = 0
    if not 1 <= index <= total:
        raise pytest.UsageError(f"--shard takes I/N with 1 <= I <= N, not {value!r}")
    return index, total


def _take_shard(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Keep part I of N: the I-th of every N selected tests, in collection order.

    Taken in turn, the parts get about the same share of each file, so a file of heavy tests
    is split too, and each part runs a file's module fixtures only for the tests it keeps.
    Every process collects in the same order, so the xdist workers of one part agree.
    """
    shard = _shard(config)
    if shard is None:
        return
    index, total = shard
    kept = [item for number, item in enumerate(items) if number % total == index - 1]
    dropped = [item for number, item in enumerate(items) if number % total != index - 1]
    if dropped:
        config.hook.pytest_deselected(items=dropped)
    items[:] = kept


# Last, so that -m and -k have deselected first: the parts share what is left.
@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    """Keep this run's shard, then give each parallel worker one long simulation to start with.

    pytest-xdist's worksteal scheduler hands each worker a contiguous block of the
    collected items and lets idle workers steal from the end of other blocks. When
    the long simulations sit together, one worker runs them back to back while the
    others finish early; putting one at the head of each block starts them all at once.
    Every worker collects in this same order, so the blocks still agree.
    """
    _take_shard(config, items)
    workers = getattr(config, "workerinput", {}).get("workercount", 1)
    long_runs = [item for item in items if item.get_closest_marker("long_simulation")]
    if workers < 2 or not long_runs:
        return
    rest = [item for item in items if item.get_closest_marker("long_simulation") is None]
    size = len(items) // workers
    ordered: list[pytest.Item] = []
    for index in range(workers):
        block = rest[: size - (1 if index < len(long_runs) else 0)] if index < workers - 1 else rest
        rest = rest[len(block) :]
        ordered.extend(long_runs[index : index + 1] + block)
    ordered.extend(long_runs[workers:])
    items[:] = ordered
