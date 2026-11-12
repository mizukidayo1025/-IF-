"""The fixture of the core's typed rows holds every typed namespace (Issue #225, F1).

`native/core/tests/typed_rows.rs` (cargo test) reads each typed namespace of
`native/core/tests/data/typed_rows.json` into its row without Python and writes it back to the
same bytes. The fixture is the worlds `support/core_typed_rows.py` builds
(`integration/test_core_fixtures.py` checks that the file is what they are today); this checks
that together they hold every typed namespace Python has.
"""

from __future__ import annotations

import json

from support.core_fixtures import fixture_path
from support.core_typed_rows import typed_namespaces


def test_the_fixture_s_worlds_hold_every_typed_namespace() -> None:
    fixture = json.loads(fixture_path("typed_rows").read_text(encoding="utf-8"))
    names = typed_namespaces()
    assert fixture["typed"] == names
    assert set(names) == {name for world in fixture["worlds"] for name in world["model_state"]}
