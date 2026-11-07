"""The VM deploy kit (deploy/vm/, docs/vm-deploy.md) matches the CLI and fixtures it uses."""

from __future__ import annotations

import html
import json
import runpy
import shlex
import sys
from pathlib import Path
from typing import Any

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


def _builder() -> dict[str, Any]:
    return runpy.run_path(str(ROOT / "scripts" / "build_project_sample.py"))


def _landing(
    argv: list[str], monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> str:
    monkeypatch.setattr(sys, "argv", ["render_landing.py", *argv])
    runpy.run_path(str(KIT / "render_landing.py"), run_name="__main__")
    return capsys.readouterr().out


def _setup_paths() -> dict[str, str]:
    """setup.sh's path variables, expanded."""
    values: dict[str, str] = {}
    for line in (KIT / "setup.sh").read_text(encoding="utf-8").splitlines():
        name, _, value = line.partition("=")
        if name in {"APP", "DATA", "SAMPLE_DIR"}:
            for known, path in values.items():
                value = value.replace(f"${known}", path)
            values[name] = value
    return values


def test_the_landing_page_links_each_slot_to_the_sample_the_builder_names(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """The summary's shape here is the builder's (integration/test_project_sample.py
    renders the page from a real build's summary)."""
    builder = _builder()
    project, run, child = builder["PROJECT_ID"], builder["RUN_ID"], builder["CHILD_ID"]
    opened = builder["OPEN_BRANCH_ID"]
    summary = tmp_path / "sample-build.json"
    summary.write_text(
        json.dumps(
            {
                "project_id": project,
                "run_id": run,
                "open_branch_id": opened,
                "agent_count": 100,
                "duration_minutes": 10080,
                "fork_minute": builder["FORK_MINUTE"],
                "intervention": {"text": builder["intervention_text"](builder["FORK_MINUTE"])},
                "branches": [
                    {"branch_id": "main", "parent_branch_id": None},
                    {"branch_id": child, "parent_branch_id": "main"},
                ],
            }
        ),
        encoding="utf-8",
    )
    page = _landing(["a<b>.example", "2", str(summary)], monkeypatch, capsys)
    path = builder["open_path"](project, run, opened)
    for n in (1, 2):
        href = html.escape(f"https://s{n}.a<b>.example{path}")
        assert f'<a href="{href}">計算済みの例をすぐ見る</a>' in page
    assert "4日目14:00" in page
    assert "計算済みの例" not in _landing(["example", "2"], monkeypatch, capsys)


def test_setup_builds_the_sample_where_reset_slot_and_the_slots_read_it() -> None:
    builder = _builder()
    setup = (KIT / "setup.sh").read_text(encoding="utf-8")
    reset = (KIT / "reset-slot.sh").read_text(encoding="utf-8")
    paths = _setup_paths()
    assert paths["APP"] + "/" == APP
    # One function runs the builder for the build and for --check, on a sample directory.
    assert '"$VENV/bin/python" "$APP/scripts/build_project_sample.py" "$dir/projects"' in setup
    assert '--summary "$dir/sample-build.json" "$@"' in setup
    assert 'sample_builder "$SAMPLE_DIR.new" >/dev/null' in setup
    # It is rebuilt only when --check does not answer current (or with SAMPLE=rebuild).
    assert 'SAMPLE_STATE="$(sample_builder "$SAMPLE_DIR" --check || true)"' in setup
    assert "if [[ $SAMPLE_STATE != current ]]; then" in setup
    # The build directory becomes SAMPLE_DIR; its Project is what both scripts copy.
    template = f"{paths['SAMPLE_DIR']}/projects/{builder['PROJECT_ID']}"
    assert f"sample={template}\n" in reset
    assert f'cp -a "$SAMPLE_DIR/projects/{builder["PROJECT_ID"]}"' in setup
    assert 'SAMPLE_SUMMARY=("$SAMPLE_DIR/sample-build.json")' in setup
    # Both copy into the store that each slot's service opens.
    unit = (KIT / "sheltersim@.service").read_text(encoding="utf-8")
    store = next(
        line.split("SHELTERSIM_PROJECTS_DIR=", 1)[1]
        for line in unit.splitlines()
        if "SHELTERSIM_PROJECTS_DIR=" in line
    )
    assert store == f"{paths['DATA']}/slot%i/demo_replay/runs/.projects"
    assert "projects=$DATA/slot$n/demo_replay/runs/.projects\n" in setup
    assert "slot=/var/lib/sheltersim/slot$n\n" in reset
    assert 'cp -a "$sample" "$slot/demo_replay/runs/.projects/sample"' in reset


def test_the_redirects_to_the_new_screen_keep_the_query() -> None:
    setup = (KIT / "setup.sh").read_text(encoding="utf-8")
    redirect = next(line for line in setup.splitlines() if "redir @older" in line)
    assert "/demo-v2{?query}" in redirect
