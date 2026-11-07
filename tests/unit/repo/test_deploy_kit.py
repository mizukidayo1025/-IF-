"""The VM deploy kit (deploy/vm/, docs/vm-deploy.md) matches the CLI and fixtures it uses."""

from __future__ import annotations

import runpy
import shlex
import sys
from pathlib import Path

import pytest

from sheltersim.cli import build_parser

ROOT = Path(__file__).parents[3]
KIT = ROOT / "deploy" / "vm"
APP = "/opt/sheltersim/app/"


def _exec_start() -> list[str]:
    unit = (KIT / "sheltersim@.service").read_text(encoding="utf-8")
    joined = unit.replace("\\\n", " ")
    line = next(row for row in joined.splitlines() if row.startswith("ExecStart="))
    command = line.removeprefix("ExecStart=").replace("%i", "1").replace("${PORT}", "8801")
    command = command.replace("${STORAGE_GB}", "14")
    return shlex.split(command)


def test_the_slot_unit_starts_sheltersim_web_with_options_the_cli_accepts() -> None:
    words = _exec_start()
    assert words[0] == "/opt/sheltersim/venv/bin/sheltersim"
    args = build_parser().parse_args(words[1:])
    assert args.command == "web"
    assert args.host == "127.0.0.1"
    assert args.max_parallel_runs == 1
    assert args.project_storage_gb == 14


def test_the_files_the_kit_reads_from_the_app_exist_in_the_repository() -> None:
    texts = [
        (KIT / "sheltersim@.service").read_text(encoding="utf-8"),
        (KIT / "reset-slot.sh").read_text(encoding="utf-8"),
    ]
    paths = {
        word.rstrip('";')
        for text in texts
        for word in text.split()
        if word.startswith(APP) or word.startswith(f'"{APP}')
    }
    assert paths
    for path in paths:
        assert (ROOT / path.strip('"').removeprefix(APP)).exists(), path


def test_the_landing_page_escapes_the_domain(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(sys, "argv", ["render_landing.py", "a<b>.example", "2"])
    runpy.run_path(str(KIT / "render_landing.py"), run_name="__main__")
    page = capsys.readouterr().out
    assert "https://s2.a&lt;b&gt;.example/demo-v2" in page
    assert "<b>" not in page
