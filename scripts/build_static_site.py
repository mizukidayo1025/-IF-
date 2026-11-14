"""Build the browser version of /demo-v2 as a static site (#266, stage A of #263).

    python scripts/build_static_site.py OUTPUT_DIR --pkg PKG_DIR --bundle SAMPLE.shelterproj

PKG_DIR is the output of `wasm-bindgen --target web` for sheltersim_wasm.wasm, and the bundle the
sample of scripts/build_browser_sample.py (docs/development.md, "ブラウザー版の静的サイト"). The
screen's files (src/sheltersim/web/static/demo-v2*) are copied unchanged; the browser build's
demo-v2.html is the server's with two more lines: the script that wraps `fetch` and answers the
screen's `/api/v1/...` in a Worker from the browser's store and WASM (native/wasm/js/api_fetch.js),
and a style that hides the link to the old screen (/demo), which the site does not have.

The site is served at the root of its domain, as the screen loads `/demo-v2.css` and the like and
`history.replaceState`s to `/demo-v2?…`:

    /                       index.html: links to the sample's Run computed by the site's tool
                            (/demo-v2?project=…&run=…&branch=main#simulation) and to the Run the
                            browser computes itself (the same with run=COMPUTE_RUN, #267)
    (a missing path)        404.html, with status 404
    /demo-v2                demo-v2.html (a host serving `/demo-v2` from `demo-v2.html`, as
                            Cloudflare Workers' static assets (deploy/workers/wrangler.jsonc, #289),
                            Cloudflare Pages, GitHub Pages and Netlify do; for others see the docs)
    /demo-v2.css, /demo-v2*.js, /demo-v2-logo.jpg   the screen's files
    /LICENSE.txt            the repository's LICENSE
    /THIRD-PARTY-NOTICES.txt
                            the licenses of the crates the WASM holds (#290), from
                            `cargo tree`/`cargo metadata` of native/ and their packages' files
    /THIRD-PARTY-NOTICES-rust-library.html
                            the Rust standard library's notices (rustc's COPYRIGHT-library.html)
    /browser/               api_fetch.js, api_worker.mjs and the modules it imports, pkg/ (WASM),
                            gsi-shelter-index.json (the shelter CSV's sha256, metadata and
                            municipality names, for the regional research; with --shelter-csv
                            or --fetch-shelters)
    /sample/sample.json     the sample's manifest (browser_api.mjs's SAMPLE_FORMAT) and its parts,
                            sample.shelterproj.000, .001, … of at most --part-mib MiB each

No file is larger than --part-mib (16 MiB by default; Cloudflare Workers' static assets take at most
25 MiB a file).
OUTPUT_DIR must be outside the repository and empty or missing: the site is generated, never
committed. scripts/serve_static_site.py serves it locally for checks.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import struct
import subprocess
import sys
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

ROOT = Path(__file__).resolve().parents[1]
STATIC = ROOT / "src" / "sheltersim" / "web" / "static"
JS = ROOT / "native" / "wasm" / "js"
# The screen's files the browser version serves as they are.
SCREEN_FILES = (
    "demo-v2.css",
    "demo-v2-logo.jpg",
    "demo-v2.js",
    "demo-v2-map.js",
    "demo-v2-scene.js",
    "demo-v2-clock.js",
    "demo-v2-replay.js",
    "demo-v2-sprites.js",
)
# The browser's API: the page's script, the Worker and the modules it imports.
BROWSER_FILES = (
    "api_fetch.js",
    "api_worker.mjs",
    "api_answers.mjs",
    "browser_api.mjs",
    "compute.mjs",
    "project_store.mjs",
    "research.mjs",
    "run_store.mjs",
    "scenario_routes.mjs",
)
# What the browser's regional research reads of the GSI national shelter CSV, which it does not
# download (no CORS; research.mjs, #300): written from a copy of the CSV (--shelter-csv) or from
# the CSV downloaded at build time (--fetch-shelters).
SHELTER_INDEX = "gsi-shelter-index.json"
PKG_FILES = ("sheltersim_wasm.js", "sheltersim_wasm_bg.wasm")
# The Run the browser makes beside the sample's, to compute itself (compute.mjs's COMPUTE_RUN).
COMPUTE_RUN = "run-browser-compute"
SAMPLE_FORMAT = "sheltersim-site-sample"
SAMPLE_VERSION = 1
# The lines the browser build adds to demo-v2.html, before the screen's first script: the script
# that answers the API, and a style that hides the link to the old screen (/demo), which the static
# site does not have.
SHIM = (
    '<script src="/browser/api_fetch.js" data-worker="/browser/api_worker.mjs"'
    ' data-sample="/sample/sample.json"></script>\n'
    '    <style>a[href="/demo"] { display: none; }</style>'
)
FIRST_SCRIPT = '<script src="/demo-v2-scene.js" defer></script>'
# The licenses of what the WASM holds (#290): its crates' in NOTICES, the Rust standard library's
# in RUST_LIBRARY_NOTICES (the notices rustc ships for programs it builds).
NOTICES = "THIRD-PARTY-NOTICES.txt"
RUST_LIBRARY_NOTICES = "THIRD-PARTY-NOTICES-rust-library.html"
WASM_CRATE = "sheltersim_wasm"
WASM_TARGET = "wasm32-unknown-unknown"
# The files of a crate's package that hold its license or notices.
LICENSE_FILE = re.compile(r"(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([-._].*)?", re.I)
# A line of `cargo tree --prefix none --format {p}`: the name, the version, the path of a crate of
# the workspace and "(*)" for one already listed.
TREE_LINE = re.compile(r"(\S+) v(\S+)(?: \([^*)][^)]*\))?(?: \(\*\))?")
# The bundle's header (native/wasm/js/project_store.mjs): magic, version, manifest length, sha256.
MAGIC = b"\x89SHELTERSIM-PRJ\n"
HEADER = len(MAGIC) + 4 + 4 + 32
LIMIT_MIB = 20


def browser_html(server_html: str) -> str:
    """The server's demo-v2.html with the browser version's script before the screen's."""
    if server_html.count(FIRST_SCRIPT) != 1:
        raise ValueError(f"demo-v2.html has not exactly one {FIRST_SCRIPT}")
    return server_html.replace(FIRST_SCRIPT, f"{SHIM}\n    {FIRST_SCRIPT}")


def bundle_project(bundle: Path) -> tuple[str, list[str]]:
    """The Project id of a bundle and its Runs' ids, read from its manifest."""
    with bundle.open("rb") as file:
        header = file.read(HEADER)
        if len(header) != HEADER or header[: len(MAGIC)] != MAGIC:
            raise ValueError(f"{bundle} is not a project bundle")
        (length,) = struct.unpack(">I", header[len(MAGIC) + 4 : len(MAGIC) + 8])
        manifest = json.loads(file.read(length))
    runs = sorted(
        {
            entry["path"].split("/")[1]
            for entry in manifest["entries"]
            if entry["path"].startswith("runs/") and entry["path"].endswith("/run.json")
        }
    )
    return manifest["project_id"], runs


NOT_FOUND = (
    "<!doctype html>\n"
    '<html lang="ja">\n<head>\n<meta charset="utf-8" />\n'
    "<title>見つかりません | 避難所IF！</title>\n</head>\n"
    '<body><p>ページが見つかりません。<a href="/">避難所IF！の計算済みの例を開く</a></p></body>\n'
    "</html>\n"
)


# The first page's reset button: it removes the Run the browser computed (its folder in the
# browser's store, api_worker.mjs's `projects` folder), which the Worker makes again, not computed,
# the next time the screen opens (compute.mjs's ensureComputeRun). The sample is kept. A page
# shown again from the back-forward cache is loaded again, so a site updated since is not left
# showing its old first page.
RESET_SCRIPT = """<script>
(() => {
  "use strict";
  const path = %s;
  addEventListener("pageshow", (event) => {
    if (event.persisted) location.reload();
  });
  const button = document.getElementById("reset");
  const out = document.getElementById("reset-note");
  button.addEventListener("click", async () => {
    const question = "このブラウザーで計算した結果を消して、0分から計算し直せるようにします。" +
      "計算済みの例は消えません。計算の画面を開いているタブは、先に閉じてください。よろしいですか？";
    if (!confirm(question)) return;
    button.disabled = true;
    out.textContent = "消しています…";
    try {
      let folder = await navigator.storage.getDirectory();
      for (const name of path.slice(0, -1)) folder = await folder.getDirectoryHandle(name);
      await folder.removeEntry(path[path.length - 1], { recursive: true });
      out.textContent = "消しました。「このブラウザーで計算する」を開くと、0分から計算できます。";
    } catch (error) {
      const name = error && error.name;
      if (name === "NotFoundError") {
        out.textContent = "まだ計算していないので、消すものはありません。";
      } else if (name === "NoModificationAllowedError" || name === "InvalidModificationError") {
        out.textContent = "計算の画面が開いているため消せません。" +
          "その画面のタブを閉じてから、もう一度押してください。";
      } else {
        out.textContent = "消せませんでした：" + ((error && error.message) || error);
      }
    } finally {
      button.disabled = false;
    }
  });
})();
</script>
"""


def index_html(sample: str, compute: str, project_id: str = "sample") -> str:
    """The site's first page: links to the sample's Run (`sample`) and to the Run the browser
    computes itself (`compute`), and a button that resets the latter."""

    def link(target: str, title: str, note: str) -> str:
        return (
            f'<a class="choice" href="{target.replace("&", "&amp;")}">'
            f"<strong>{title}</strong><span>{note}</span></a>\n"
        )

    return (
        "<!doctype html>\n"
        '<html lang="ja">\n<head>\n<meta charset="utf-8" />\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1" />\n'
        "<title>避難所IF！</title>\n"
        "<style>\n"
        ":root { color-scheme: light dark; --ink: #1d2733; --paper: #f6f7f9; --card: #fff;"
        " --line: #cfd6de; --accent: #0b5cad; }\n"
        "@media (prefers-color-scheme: dark) { :root { --ink: #e6ebf0; --paper: #14191f;"
        " --card: #1d242c; --line: #3a4652; --accent: #7db7ff; } }\n"
        "body { margin: 0; padding: 32px 16px; background: var(--paper); color: var(--ink);"
        " font-family: system-ui, sans-serif; line-height: 1.6; }\n"
        "main { max-width: 640px; margin: 0 auto; }\n"
        "h1 { font-size: 1.6rem; margin: 0 0 4px; }\n"
        ".choice { display: block; margin: 16px 0; padding: 16px; border: 1px solid var(--line);"
        " border-radius: 8px; background: var(--card); color: inherit; text-decoration: none; }\n"
        ".choice:hover, .choice:focus { border-color: var(--accent); }\n"
        ".choice strong { display: block; color: var(--accent); font-size: 1.1rem; }\n"
        "small { display: block; margin-top: 24px; opacity: 0.8; }\n"
        ".reset { margin: 8px 0 0; }\n"
        ".reset button { font: inherit; padding: 6px 14px; border: 1px solid var(--line);"
        " border-radius: 6px; background: var(--card); color: inherit; cursor: pointer; }\n"
        ".reset button:disabled { opacity: 0.6; cursor: default; }\n"
        "</style>\n"
        "</head>\n<body>\n<main>\n<h1>避難所IF！</h1>\n"
        "<p>合成の避難所（100人・7日間）のシミュレーションです。計算も表示も、このブラウザーの中で動きます。</p>\n"
        + link(
            sample,
            "計算済みの例を見る",
            "あらかじめ計算した7日分を、地図と指標で再生します。",
        )
        + link(
            compute,
            "このブラウザーで計算する",
            "同じシナリオを0分から計算します。「終了まで計算」を押すと、"
            "時計が進むのを見ながら7日分を計算できます（数十秒）。",
        )
        + '<div class="reset"><button id="reset" type="button">計算をやり直す</button>'
        "<span>（このブラウザーで計算した結果を消し、0分に戻します）</span>"
        '<p id="reset-note" role="status"></p></div>\n'
        + "<small>初めて開くときは、例のデータ（約47 MB）を読み込んで、このブラウザーに保存します。"
        "実在の避難所や人の記録ではありません。</small>\n"
        '<small><a href="/LICENSE.txt">ライセンス</a>・'
        f'<a href="/{NOTICES}">外部の部品のライセンス</a>'
        f'（<a href="/{RUST_LIBRARY_NOTICES}">Rustの標準ライブラリー</a>）</small>\n'
        "</main>\n"
        + RESET_SCRIPT % json.dumps(["projects", project_id, "runs", COMPUTE_RUN])
        + "</body>\n</html>\n"
    )


def split_bundle(bundle: Path, directory: Path, part_bytes: int) -> dict[str, Any]:
    """Write the bundle's parts and their manifest to `directory`; the manifest."""
    project_id, _ = bundle_project(bundle)
    whole = hashlib.sha256()
    parts = []
    total = 0
    with bundle.open("rb") as file:
        while chunk := file.read(part_bytes):
            name = f"sample.shelterproj.{len(parts):03d}"
            (directory / name).write_bytes(chunk)
            whole.update(chunk)
            parts.append(
                {"path": name, "bytes": len(chunk), "sha256": hashlib.sha256(chunk).hexdigest()}
            )
            total += len(chunk)
    manifest = {
        "format": SAMPLE_FORMAT,
        "version": SAMPLE_VERSION,
        "project_id": project_id,
        "bytes": total,
        "sha256": whole.hexdigest(),
        "parts": parts,
    }
    (directory / "sample.json").write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    return manifest


def shelter_index(
    *, csv_path: Path | None = None, fetch: bool = False, retrieved_at: str | None = None
) -> dict[str, Any] | None:
    """`gsi_shelter_index` of a copy of the national shelter CSV (read now, without HTTP
    metadata) or of the CSV downloaded now; None with neither. `retrieved_at` (ISO 8601 with a
    time zone) is the copy's retrieval time instead of now: a check pins it to the Python side's
    clock (benchmarks/static_site_e2e.py), as the research's time is part of the Scenario."""
    from datetime import UTC, datetime

    from sheltersim.region import providers

    if csv_path is not None:
        raw = csv_path.read_bytes()
        try:
            text = raw.decode("utf-8-sig")
        except UnicodeDecodeError as error:
            raise ValueError("the shelter CSV is not UTF-8") from error
        when = datetime.fromisoformat(retrieved_at) if retrieved_at else datetime.now(UTC)
        if when.tzinfo is None:
            raise ValueError("--shelter-retrieved-at needs a time zone")
        snapshot = providers.GsiCsvSnapshot(
            sha256=hashlib.sha256(raw).hexdigest(), retrieved_at=when.astimezone(UTC), text=text
        )
    elif fetch:
        snapshot = providers._get_gsi_shelter_snapshot(providers._GSI_SHELTER_CSV_URL)
    else:
        return None
    try:
        return providers.gsi_shelter_index(snapshot)
    except providers.RegionProviderError as error:
        raise ValueError(str(error)) from error


# A module's static imports of modules beside it (`import … from "./x.mjs"`, `import "./x.mjs"`).
RELATIVE_IMPORT = re.compile(r"""\bimport\b[^;'"]*?['"]\./([A-Za-z0-9_.-]+\.m?js)['"]""")


def browser_modules() -> list[str]:
    """BROWSER_FILES and the modules they import beside them, at any depth: a module the Worker
    imports is on the site even when the list above does not name it."""
    names = list(BROWSER_FILES)
    for name in names:
        for imported in RELATIVE_IMPORT.findall((JS / name).read_text(encoding="utf-8")):
            if imported not in names:
                names.append(imported)
    return names


def _output(*command: str) -> str:
    """What `command` writes; ValueError with what it wrote to stderr if it fails."""
    result = subprocess.run(command, capture_output=True, encoding="utf-8")
    if result.returncode != 0:
        raise ValueError(f"{' '.join(command[:2])} failed: {result.stderr.strip()}")
    return result.stdout


def _cargo(*arguments: str) -> str:
    manifest = str(ROOT / "native" / "Cargo.toml")
    return _output("cargo", *arguments, "--manifest-path", manifest, "--locked")


def wasm_crates() -> list[tuple[str, str]]:
    """The (name, version) of each crate the WASM links, as its build resolves them: the normal
    dependencies of sheltersim_wasm for the browser's target, at any depth, but proc-macro crates,
    which run in the compiler, and what only they use."""
    lines = _cargo(
        "tree",
        "-p",
        WASM_CRATE,
        "--target",
        WASM_TARGET,
        "-e",
        "normal,no-proc-macro",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ).splitlines()
    crates = set()
    for line in filter(None, lines):
        match = TREE_LINE.fullmatch(line)
        if match is None:
            raise ValueError(f"cargo tree wrote {line!r}")
        crates.add((match[1], match[2]))
    return sorted(crates)


def crate_notices(metadata: dict[str, Any], crates: list[tuple[str, str]]) -> str:
    """NOTICES: each crate of `crates` but the workspace's own (MIT, /LICENSE.txt) with the
    license its Cargo.toml states and its repository, then the texts of the license and notice
    files its package holds, each text once. A crate without such a file is refused."""
    members = set(metadata["workspace_members"])
    packages: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for package in metadata["packages"]:
        packages.setdefault((package["name"], package["version"]), []).append(package)
    entries, texts, bare = [], {}, []
    for name, version in crates:
        found = packages.get((name, version), [])
        if len(found) != 1:
            raise ValueError(f"cargo metadata has {len(found)} packages {name} {version}")
        package = found[0]
        if package["id"] in members:
            continue
        directory = Path(package["manifest_path"]).parent
        files = sorted(
            path
            for path in directory.iterdir()
            if path.is_file() and LICENSE_FILE.fullmatch(path.name)
        )
        if package.get("license_file"):
            named = (directory / package["license_file"]).resolve()
            if named not in [path.resolve() for path in files]:
                files.append(named)
        if not files:
            bare.append(f"{name} {version}")
            continue
        labels = []
        for path in files:
            text = path.read_text(encoding="utf-8").replace("\r\n", "\n").strip("\n")
            number = texts.setdefault(text, len(texts) + 1)
            labels.append(f"{path.name} [{number}]")
        entries.append(
            f"{name} {version}\n"
            f"  License: {package.get('license') or 'see its files'}\n"
            f"  Repository: {package.get('repository') or '-'}\n"
            f"  Files: {', '.join(labels)}\n"
        )
    if bare:
        raise ValueError(f"no license file in the packages of {', '.join(bare)}")
    return (
        "Third-party notices of the browser version of 避難所IF！ (#290)\n\n"
        "The site's WASM (/browser/pkg/sheltersim_wasm_bg.wasm) and its JS glue\n"
        "(/browser/pkg/sheltersim_wasm.js, which wasm-bindgen writes) are built from the\n"
        "repository's crates (MIT, /LICENSE.txt), the crates below and the Rust standard\n"
        f"library, whose notices are in /{RUST_LIBRARY_NOTICES}.\n"
        "Each crate is listed with the license its Cargo.toml states and the license and notice\n"
        "files its package holds; the texts of those files follow the list.\n\n"
        "Crates\n======\n\n"
        + "\n".join(entries)
        + "\nTexts\n=====\n"
        + "".join(f"\n[{number}]\n\n{text}\n" for text, number in texts.items())
    )


def third_party_notices() -> dict[str, bytes]:
    """The site's NOTICES and RUST_LIBRARY_NOTICES, from the Cargo.lock of native/ and the
    toolchain of `cargo` (the one the WASM is built with)."""
    metadata = json.loads(
        _cargo("metadata", "--format-version", "1", "--filter-platform", WASM_TARGET)
    )
    sysroot = _output("rustc", "--print", "sysroot").strip()
    library = Path(sysroot) / "share" / "doc" / "rust" / "COPYRIGHT-library.html"
    return {
        NOTICES: crate_notices(metadata, wasm_crates()).encode(),
        RUST_LIBRARY_NOTICES: library.read_bytes(),
    }


def build(
    output: Path,
    *,
    pkg: Path,
    bundle: Path,
    run_id: str | None = None,
    branch_id: str = "main",
    part_mib: int = 16,
    shelters: dict[str, Any] | None = None,
    notices: dict[str, bytes],
) -> dict[str, Any]:
    """Assemble the site in `output`; a summary of what it holds. `notices` are the files
    third_party_notices() writes, by name."""
    output = output.resolve()
    if output == ROOT or ROOT in output.parents:
        raise ValueError("the site is generated: write it outside the repository")
    if output.exists() and any(output.iterdir()):
        raise ValueError(f"{output} is not empty")
    if not 1 <= part_mib <= LIMIT_MIB:
        raise ValueError(f"--part-mib must be from 1 to {LIMIT_MIB}")
    project_id, runs = bundle_project(bundle)
    if run_id is None:
        if len(runs) != 1:
            raise ValueError(f"the bundle has the Runs {runs}: name one with --run")
        run_id = runs[0]
    elif run_id not in runs:
        raise ValueError(f"the bundle has no Run {run_id}")
    output.mkdir(parents=True, exist_ok=True)
    for name in SCREEN_FILES:
        shutil.copyfile(STATIC / name, output / name)
    (output / "demo-v2.html").write_text(
        browser_html((STATIC / "demo-v2.html").read_text(encoding="utf-8")), encoding="utf-8"
    )
    (output / "browser" / "pkg").mkdir(parents=True)
    for name in browser_modules():
        shutil.copyfile(JS / name, output / "browser" / name)
    for name in PKG_FILES:
        shutil.copyfile(pkg / name, output / "browser" / "pkg" / name)
    if shelters is not None:
        (output / "browser" / SHELTER_INDEX).write_text(
            json.dumps(shelters, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
            encoding="utf-8",
        )
    (output / "sample").mkdir()
    manifest = split_bundle(bundle, output / "sample", part_mib * 2**20)
    query = urlencode({"project": project_id, "run": run_id, "branch": branch_id})
    target = f"/demo-v2?{query}#simulation"
    compute_query = urlencode({"project": project_id, "run": COMPUTE_RUN, "branch": "main"})
    compute = f"/demo-v2?{compute_query}#simulation"
    (output / "index.html").write_text(index_html(target, compute, project_id), encoding="utf-8")
    # A missing file is a 404 (Workers' static assets answer with it under deploy/workers/'s
    # not_found_handling "404-page"; Cloudflare Pages without a 404.html answers every path with
    # the root's index.html, as for a single-page app).
    (output / "404.html").write_text(NOT_FOUND, encoding="utf-8")
    # The repository's license (MIT) travels with the copy of its code the site is.
    shutil.copyfile(ROOT / "LICENSE", output / "LICENSE.txt")
    # And the licenses of what the WASM holds (#290).
    if set(notices) != {NOTICES, RUST_LIBRARY_NOTICES}:
        raise ValueError(f"the notices are {sorted(notices)}")
    for name, data in notices.items():
        (output / name).write_bytes(data)
    # GitHub Pages: serve the files as they are, without Jekyll.
    (output / ".nojekyll").write_bytes(b"")
    files = sorted(path for path in output.rglob("*") if path.is_file())
    largest = max(files, key=lambda path: path.stat().st_size)
    if largest.stat().st_size > LIMIT_MIB * 2**20:
        raise ValueError(f"{largest} is larger than {LIMIT_MIB} MiB")
    return {
        "output": str(output),
        "open": target,
        "compute": compute,
        "files": len(files),
        "bytes": sum(path.stat().st_size for path in files),
        "largest": {
            "path": str(largest.relative_to(output)),
            "bytes": largest.stat().st_size,
        },
        "sample": {key: manifest[key] for key in ("project_id", "bytes", "sha256")}
        | {"parts": len(manifest["parts"])},
        "shelter_index": None
        if shelters is None
        else {key: shelters[key] for key in ("sha256", "retrieved_at", "invalid_record_count")}
        | {"municipality_codes": len(shelters["codes"])},
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("output", type=Path, help="the site's directory (empty or missing)")
    parser.add_argument(
        "--pkg", type=Path, required=True, help="the output of wasm-bindgen --target web"
    )
    parser.add_argument(
        "--bundle", type=Path, required=True, help="the sample (build_browser_sample.py)"
    )
    parser.add_argument("--run", help="the Run the index page opens (the bundle's only one)")
    parser.add_argument("--branch", default="main", help="the branch the index page opens")
    parser.add_argument(
        "--part-mib", type=int, default=16, help="the largest part of the sample, in MiB"
    )
    shelter = parser.add_mutually_exclusive_group()
    shelter.add_argument(
        "--shelter-csv", type=Path, help="a copy of the GSI national shelter CSV (mergeFromCity_1)"
    )
    parser.add_argument(
        "--shelter-retrieved-at",
        help="with --shelter-csv: the copy's retrieval time (ISO 8601), e.g. a pinned clock",
    )
    shelter.add_argument(
        "--fetch-shelters",
        action="store_true",
        help="download the GSI national shelter CSV now (11.7 MB)",
    )
    arguments = parser.parse_args()
    try:
        shelters = shelter_index(
            csv_path=arguments.shelter_csv,
            fetch=arguments.fetch_shelters,
            retrieved_at=arguments.shelter_retrieved_at,
        )
        if shelters is None:
            print(
                "no shelter index: the site's Scenario preview answers browser_unsupported",
                file=sys.stderr,
            )
        notices = third_party_notices()
        summary = build(
            arguments.output,
            pkg=arguments.pkg,
            bundle=arguments.bundle,
            run_id=arguments.run,
            branch_id=arguments.branch,
            part_mib=arguments.part_mib,
            shelters=shelters,
            notices=notices,
        )
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 2
    json.dump(summary, sys.stdout, ensure_ascii=False, indent=1)
    print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
