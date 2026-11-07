"""Print the judges' landing page: python3 render_landing.py <domain> <slots>."""

import html
import sys

domain, slots = sys.argv[1], int(sys.argv[2])
links = "\n".join(
    f'<li><a href="https://s{n}.{html.escape(domain)}/demo-v2">環境 {n}</a>'
    f' <span class="sub">(<a href="https://s{n}.{html.escape(domain)}/demo">'
    "従来の画面</a>)</span></li>"
    for n in range(1, slots + 1)
)
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
a {{ color:var(--accent); }} .sub, .note {{ color:var(--muted); font-size:.9rem; }}
li {{ margin:.4rem 0; }}
</style></head><body>
<h1>避難所IF！ 審査用環境</h1>
<p>番号を1つ選んで開いてください。環境ごとにデータが分かれています。
<strong>ほかの方と同じ番号を使うと操作が混ざる</strong>ため、
使用中のようでしたら別の番号をお選びください。</p>
<ol>
{links}
</ol>
<p class="note">7日分の事前計算結果と比較をすぐに表示できます。新しく計算する場合、
7日分は数分かかります。AI機能の利用額には上限があり、上限に達すると
AIを使わない処理に切り替わります。表示されるデータはすべて合成データです。</p>
</body></html>""")
