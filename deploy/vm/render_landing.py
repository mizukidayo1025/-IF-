"""Print the judges' landing page: python3 render_landing.py <domain> <slots> [sample-build.json].

With the summary that scripts/build_project_sample.py wrote, each slot also links to the
precomputed sample in the new screen, and the page gives the computing time per day measured
on this VM by that build: its build_seconds over the minutes it computed (the main branch and
the branch from its fork minute). Without it, the page gives a time measured elsewhere.
"""

import html
import json
import math
import sys
from pathlib import Path
from urllib.parse import urlencode

# Without a build on this VM: a 100-person generated Scenario on a 16-thread PC took about
# 11 s a day and 93 s for 7 days (#80); a VM's vCPUs are slower, so twice that.
FALLBACK_SECONDS_PER_DAY = 25.0


def duration_text(seconds: float) -> str:
    # Rounded up, so that the page does not promise less than it takes.
    if seconds < 90:
        return f"約{max(5, math.ceil(seconds / 5) * 5)}秒"
    return f"約{math.ceil(seconds / 60)}分"


def compute_note(seconds_per_day: float, measured: bool) -> str:
    where = "（この環境で計測した目安）" if measured else "（目安）"
    return (
        f"新しく計算する場合、1日分は{duration_text(seconds_per_day)}、"
        f"7日分は{duration_text(seconds_per_day * 7)}程度かかります{where}。"
        "ほかの環境で同時に計算していると、長くなることがあります。"
    )


domain, slots = sys.argv[1], int(sys.argv[2])
sample_path, sample_note = None, ""
seconds_per_day, measured = FALLBACK_SECONDS_PER_DAY, False
if len(sys.argv) > 3:
    sample = json.loads(Path(sys.argv[3]).read_text(encoding="utf-8"))
    query = urlencode(
        {
            "project": sample["project_id"],
            "run": sample["run_id"],
            "branch": sample["open_branch_id"],
        }
    )
    sample_path = f"/demo-v2?{query}#simulation"
    fork = sample["fork_minute"]
    sample_note = html.escape(
        f"「計算済みの例をすぐ見る」は、{sample['agent_count']}人・"
        f"{sample['duration_minutes'] // 1440}日間の合成シナリオと、{fork // 1440 + 1}日目"
        f"{fork % 1440 // 60:02d}:{fork % 60:02d}に「{sample['intervention']['text']}」"
        "を承認した分岐を、あらかじめ計算した結果です。計算を待たずに再生でき、"
        "分岐比較の画面で「比較を更新」を押すと2つを比べられます。"
    )
    computed = 2 * sample["duration_minutes"] - fork
    if sample.get("build_seconds") and computed > 0:
        seconds_per_day, measured = sample["build_seconds"] * 1440 / computed, True


def slot_item(n: int) -> str:
    base = f"https://s{n}.{domain}"
    item = f'<a href="{html.escape(base)}/demo-v2">環境 {n}</a>'
    if sample_path is not None:
        item += f'（<a href="{html.escape(base + sample_path)}">計算済みの例をすぐ見る</a>）'
    return f"<li>{item}</li>"


links = "\n".join(slot_item(n) for n in range(1, slots + 1))
print(f"""<!doctype html>
<html lang="ja"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>避難所IF！ 審査用環境</title>
<style>
:root {{ --bg:#fff; --fg:#1b1f24; --muted:#59636e; --accent:#0b62c4; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --bg:#14171a; --fg:#e6e9ec; --muted:#9aa4ae; --accent:#6aa9f2; }}
}}
body {{ background:var(--bg); color:var(--fg); font:16px/1.7 system-ui, sans-serif;
       max-width:44rem; margin:2rem auto; padding:0 16px; }}
a {{ color:var(--accent); }} .note {{ color:var(--muted); font-size:.9rem; }}
li {{ margin:.4rem 0; }}
</style></head><body>
<h1>避難所IF！ 審査用環境</h1>
<p>番号を1つ選んで開いてください。環境ごとにデータが分かれています。
<strong>ほかの方と同じ番号を使うと操作が混ざる</strong>ため、
使用中のようでしたら別の番号をお選びください。</p>
<ol>
{links}
</ol>
<p class="note">{sample_note}{html.escape(compute_note(seconds_per_day, measured))}
AI機能の利用額には上限があり、上限に達すると
AIを使わない処理に切り替わります。表示されるデータはすべて合成データです。</p>
</body></html>""")
