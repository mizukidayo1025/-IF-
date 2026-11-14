"""The browser site's deployment (#289): .github/workflows/deploy-browser.yml builds the site as CI
builds the WASM and publishes it with deploy/workers/wrangler.jsonc (docs/development.md,
"ブラウザー版の公開"). The workflow runs only by hand, so these keep it in step with the files it
shares with CI and the docs; the config's answers are checked in
tests/integration/test_static_site.py.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).parents[3]
WORKFLOWS = ROOT / ".github" / "workflows"
DEPLOY = WORKFLOWS / "deploy-browser.yml"


def _bindgen(text: str) -> tuple[str, str]:
    version = re.search(r"^\s*version=(\S+)$", text, re.M)
    digest = re.search(r'echo "([0-9a-f]{64})  \$RUNNER_TEMP/\$archive\.tar\.gz"', text)
    assert version and digest
    return version.group(1), digest.group(1)


def test_the_workflow_builds_with_the_wasm_bindgen_ci_checks() -> None:
    deploy = _bindgen(DEPLOY.read_text("utf-8"))
    assert deploy == _bindgen((WORKFLOWS / "ci.yml").read_text("utf-8"))
    crate = (ROOT / "native" / "wasm" / "Cargo.toml").read_text("utf-8")
    assert f'wasm-bindgen = "={deploy[0]}"' in crate


def test_the_workflow_publishes_only_main_with_the_repository_s_config() -> None:
    text = DEPLOY.read_text("utf-8")
    assert re.search(r"^on:\n  workflow_dispatch:\n", text, re.M)
    assert "pull_request" not in text and "push:" not in text
    assert 'if [ "$GITHUB_REF" != refs/heads/main ]' in text
    calls = re.findall(r'npx --yes "\$WRANGLER" deploy ([^\n]*)', text)
    assert len(calls) == 2
    site = '--config deploy/workers/wrangler.jsonc --assets "$RUNNER_TEMP/site"'
    for call in calls:
        assert call.startswith(site)
    assert calls[0].endswith("--dry-run")
    # The token's value goes only to the step that sends the site.
    assert text.count("secrets.CLOUDFLARE_API_TOKEN }}") == 1


def test_the_checked_url_is_the_config_s_worker_and_the_docs_url() -> None:
    config = json.loads((ROOT / "deploy" / "workers" / "wrangler.jsonc").read_text("utf-8"))
    url = re.search(r"^  SITE_URL: (\S+)$", DEPLOY.read_text("utf-8"), re.M)
    assert url
    assert url.group(1).startswith(f"https://{config['name']}.")
    assert url.group(1).endswith(".workers.dev")
    assert f"URL={url.group(1)}\n" in (ROOT / "docs" / "development.md").read_text("utf-8")
