"""The fixture of the core's typed rows is still what its exporter writes (Issue #225, F1).

`native/core/tests/typed_rows.rs` (cargo test) reads each typed namespace of
`native/core/tests/data/typed_rows.json` into its row without Python and writes it back to the
same bytes. This checks the other side: the fixture is still the worlds
`tests/support/core_typed_rows.py` builds, and they hold every typed namespace.
`benchmarks/core_typed_rows_fixture.py` writes the fixture again after a change that moves them.
"""

from __future__ import annotations

import json

from support.core_typed_rows import typed_namespaces, typed_rows_text
from support.paths import CORE_TYPED_ROWS_FIXTURE


def test_the_fixture_is_the_worlds_of_its_exporter() -> None:
    # The text, not the values: 1 == 1.0 in Python, and the core reads them as different types.
    text = CORE_TYPED_ROWS_FIXTURE.read_text(encoding="utf-8")
    assert text == typed_rows_text()
    fixture = json.loads(text)
    names = typed_namespaces()
    assert fixture["typed"] == names
    assert set(names) == {name for world in fixture["worlds"] for name in world["model_state"]}
