"""`sheltersim web` passes its options to the server."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from sheltersim import cli
from sheltersim.web import server


def _served(monkeypatch: pytest.MonkeyPatch, *args: str) -> dict[str, Any]:
    calls: list[dict[str, Any]] = []

    def record(runs_directory: Path, **options: Any) -> None:
        calls.append({"runs_directory": runs_directory, **options})

    monkeypatch.setattr(server, "serve_inspector", record)
    assert cli.main(["web", "--runs-dir", "runs", *args]) == 0
    assert len(calls) == 1
    return calls[0]


def test_web_keeps_ten_gb_of_saved_data_by_default(monkeypatch: pytest.MonkeyPatch) -> None:
    assert _served(monkeypatch)["project_storage_bytes"] == 10_000_000_000


def test_web_sets_the_saved_data_limit_in_gb(monkeypatch: pytest.MonkeyPatch) -> None:
    assert _served(monkeypatch, "--project-storage-gb", "14")["project_storage_bytes"] == (
        14_000_000_000
    )


@pytest.mark.parametrize("value", ["0", "-1", "1.5", "ten"])
def test_web_refuses_a_limit_that_is_not_a_positive_whole_number(
    monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    monkeypatch.setattr(server, "serve_inspector", lambda *_, **__: None)
    with pytest.raises(SystemExit):
        cli.main(["web", "--project-storage-gb", value])
