"""The browser version's static site (#266): scripts/build_static_site.py assembles it and
scripts/serve_static_site.py serves it as a static host would.

The site is built from a stand-in WASM package and a small bundle (only its header and manifest
are read; the browser checks the rest): the screen's files are copied unchanged, demo-v2.html
gains the one line that loads the browser's API, the bundle is split into parts no larger than
asked that join back to it, and `/` goes to the sample's Run. The server answers `/demo-v2` with
demo-v2.html and modules and WASM with their types over HTTP. The browser's half is checked by
native/wasm/tests/node_browser_api.mjs and docs/development.md's browser checks.
"""

from __future__ import annotations

import hashlib
import json
import runpy
import struct
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).parents[2]
STATIC = ROOT / "src" / "sheltersim" / "web" / "static"


@pytest.fixture(scope="module")
def builder() -> dict[str, Any]:
    return runpy.run_path(str(ROOT / "scripts" / "build_static_site.py"))


@pytest.fixture(scope="module")
def server_module() -> dict[str, Any]:
    return runpy.run_path(str(ROOT / "scripts" / "serve_static_site.py"))


def _bundle(path: Path, size: int) -> bytes:
    """A file with a project bundle's header and manifest (Project `sample`, one Run), padded."""
    manifest = json.dumps(
        {
            "format": "sheltersim-project-bundle",
            "version": 1,
            "project_id": "sample",
            "source": None,
            "entries": [
                {"path": "project.json", "length": 0, "sha256": "0" * 64},
                {"path": "runs/run-x/run.json", "length": 0, "sha256": "0" * 64},
                {"path": "runs/run-x/branches/main/branch.json", "length": 0, "sha256": "0" * 64},
            ],
        }
    ).encode()
    head = b"\x89SHELTERSIM-PRJ\n" + struct.pack(">II", 1, len(manifest))
    data = head + hashlib.sha256(manifest).digest() + manifest
    data += bytes(index % 251 for index in range(size - len(data)))
    path.write_bytes(data)
    return data


@pytest.fixture
def site(builder: dict[str, Any], tmp_path: Path) -> tuple[Path, bytes, dict[str, Any]]:
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "sheltersim_wasm.js").write_text("export default async function init() {}\n")
    (pkg / "sheltersim_wasm_bg.wasm").write_bytes(b"\0asm\1\0\0\0")
    bundle = _bundle(tmp_path / "sample.shelterproj", 2 * 2**20 + 12345)
    output = tmp_path / "site"
    summary = builder["build"](output, pkg=pkg, bundle=tmp_path / "sample.shelterproj", part_mib=1)
    return output, bundle, summary


def test_the_site_holds_the_screen_the_browser_api_and_the_sample_in_parts(
    site: tuple[Path, bytes, dict[str, Any]], builder: dict[str, Any]
) -> None:
    output, bundle, summary = site
    RELATIVE_IMPORT, JS = builder["RELATIVE_IMPORT"], builder["JS"]
    for name in ("demo-v2.css", "demo-v2.js", "demo-v2-map.js", "demo-v2-logo.jpg"):
        assert (output / name).read_bytes() == (STATIC / name).read_bytes()
    server_html = (STATIC / "demo-v2.html").read_text(encoding="utf-8")
    browser_html = (output / "demo-v2.html").read_text(encoding="utf-8")
    added = [line for line in browser_html.splitlines() if line not in server_html.splitlines()]
    assert added == [
        '    <script src="/browser/api_fetch.js" data-worker="/browser/api_worker.mjs"'
        ' data-sample="/sample/sample.json"></script>',
        '    <style>a[href="/demo"] { display: none; }</style>',
    ]
    assert '<a href="/demo">' in server_html
    assert (output / "LICENSE.txt").read_bytes() == (STATIC.parents[3] / "LICENSE").read_bytes()
    assert browser_html.index("/browser/api_fetch.js") < browser_html.index("/demo-v2-scene.js")
    for name in ("api_fetch.js", "api_worker.mjs", "browser_api.mjs", "compute.mjs"):
        assert (output / "browser" / name).is_file()
    # Every module the site's modules import beside them is on the site (a missing one stops the
    # Worker from starting).
    browser = output / "browser"
    for module in browser.glob("*.*js"):
        for name in RELATIVE_IMPORT.findall(module.read_text(encoding="utf-8")):
            assert (browser / name).read_bytes() == (JS / name).read_bytes(), (module.name, name)
    assert (output / "browser" / "pkg" / "sheltersim_wasm_bg.wasm").is_file()

    manifest = json.loads((output / "sample" / "sample.json").read_text(encoding="utf-8"))
    assert manifest["format"] == "sheltersim-site-sample" and manifest["version"] == 1
    assert manifest["project_id"] == "sample" and manifest["bytes"] == len(bundle)
    assert manifest["sha256"] == hashlib.sha256(bundle).hexdigest()
    parts = [(output / "sample" / part["path"]).read_bytes() for part in manifest["parts"]]
    assert len(parts) == 3 and all(len(part) <= 2**20 for part in parts)
    assert b"".join(parts) == bundle
    assert [hashlib.sha256(part).hexdigest() for part in parts] == [
        part["sha256"] for part in manifest["parts"]
    ]
    assert summary["open"] == "/demo-v2?project=sample&run=run-x&branch=main#simulation"
    assert summary["compute"] == (
        "/demo-v2?project=sample&run=run-browser-compute&branch=main#simulation"
    )
    index = (output / "index.html").read_text(encoding="utf-8")
    for target in (summary["open"], summary["compute"]):
        assert f'href="{target.replace("&", "&amp;")}"' in index
    # The reset button removes the folder of that Run in the Worker's store (api_worker.mjs).
    assert '<button id="reset" type="button">計算をやり直す</button>' in index
    assert 'const path = ["projects", "sample", "runs", "run-browser-compute"];' in index
    worker_js = (STATIC.parents[3] / "native" / "wasm" / "js" / "api_worker.mjs").read_text("utf-8")
    assert 'root.getDirectoryHandle("projects", { create: true })' in worker_js
    # The Run the browser computes is the one compute.mjs makes.
    compute_js = (STATIC.parents[3] / "native" / "wasm" / "js" / "compute.mjs").read_text("utf-8")
    assert 'export const COMPUTE_RUN = "run-browser-compute";' in compute_js
    assert summary["largest"]["bytes"] <= 2**20


def test_the_site_is_built_only_outside_the_repository_into_an_empty_directory(
    builder: dict[str, Any], site: tuple[Path, bytes, dict[str, Any]], tmp_path: Path
) -> None:
    output, _, _ = site
    arguments = {"pkg": output / "browser" / "pkg", "bundle": tmp_path / "sample.shelterproj"}
    with pytest.raises(ValueError, match="outside the repository"):
        builder["build"](ROOT / "site-out", **arguments)
    with pytest.raises(ValueError, match="not empty"):
        builder["build"](output, **arguments)
    with pytest.raises(ValueError, match="no Run"):
        builder["build"](tmp_path / "other", run_id="nope", **arguments)
    with pytest.raises(ValueError, match="--part-mib"):
        builder["build"](tmp_path / "third", part_mib=21, **arguments)
    assert not (ROOT / "site-out").exists()


def test_the_site_holds_the_shelter_index_the_browser_s_research_reads(
    builder: dict[str, Any], site: tuple[Path, bytes, dict[str, Any]], tmp_path: Path
) -> None:
    """With a copy of the national shelter CSV, the site holds its `gsi_shelter_index` (#300);
    without one it holds none."""
    from support.core_research import EXTRA_CSV, shelter_index

    output, _, summary = site
    assert summary["shelter_index"] is None
    assert not (output / "browser" / "gsi-shelter-index.json").exists()
    csv_path = tmp_path / "mergeFromCity_1.csv"
    csv_path.write_bytes(b"\xef\xbb\xbf" + EXTRA_CSV.encode("utf-8"))
    made = builder["shelter_index"](csv_path=csv_path)
    expected = shelter_index(EXTRA_CSV)
    assert made["sha256"] == hashlib.sha256(csv_path.read_bytes()).hexdigest()
    assert {key: value for key, value in made.items() if key not in ("sha256", "retrieved_at")} == {
        key: value for key, value in expected.items() if key not in ("sha256", "retrieved_at")
    }
    arguments = {"pkg": output / "browser" / "pkg", "bundle": tmp_path / "sample.shelterproj"}
    with_index = builder["build"](tmp_path / "with-index", shelters=made, **arguments)
    written = tmp_path / "with-index" / "browser" / "gsi-shelter-index.json"
    assert json.loads(written.read_text(encoding="utf-8")) == made
    assert with_index["shelter_index"]["municipality_codes"] == len(made["codes"])
    assert (tmp_path / "with-index" / "browser" / "research.mjs").is_file()
    assert builder["shelter_index"]() is None
    pinned = builder["shelter_index"](csv_path=csv_path, retrieved_at="2026-11-01T12:00:00+09:00")
    assert pinned["retrieved_at"] == "2026-11-01T03:00:00Z"
    csv_path.write_bytes(b"not,a,shelter,csv\n")
    with pytest.raises(ValueError, match="schema"):
        builder["shelter_index"](csv_path=csv_path)


def _get(url: str) -> tuple[int, str, bytes]:
    try:
        with urllib.request.urlopen(url, timeout=10) as response:
            return response.status, response.headers["Content-Type"], response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers["Content-Type"], error.read()


def test_the_local_server_answers_as_a_static_host(
    server_module: dict[str, Any], site: tuple[Path, bytes, dict[str, Any]]
) -> None:
    output, _, _ = site
    resolve = server_module["resolve"]
    assert resolve(output, "/") == output / "index.html"
    assert resolve(output, "/demo-v2?project=sample") == output / "demo-v2.html"
    assert resolve(output, "/demo-v2.css") == output / "demo-v2.css"
    for path in ("/../secret", "/browser/../../x", "/nope", "/browser/", "/demo-v2/"):
        assert resolve(output, path) is None, path
    server = server_module["start"](output)
    try:
        base = f"http://127.0.0.1:{server.server_port}"
        status, kind, body = _get(base + "/demo-v2?project=sample&run=run-x")
        assert (status, kind) == (200, "text/html; charset=utf-8")
        assert body == (output / "demo-v2.html").read_bytes()
        assert _get(base + "/browser/api_worker.mjs")[1] == "text/javascript; charset=utf-8"
        assert _get(base + "/browser/pkg/sheltersim_wasm_bg.wasm")[1] == "application/wasm"
        assert _get(base + "/sample/sample.json")[1] == "application/json; charset=utf-8"
        status, kind, body = _get(base + "/api/v1/projects")
        assert (status, kind) == (404, "text/html; charset=utf-8")
        assert body == (output / "404.html").read_bytes()
    finally:
        server.shutdown()
        server.server_close()
