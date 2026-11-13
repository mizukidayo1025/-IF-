# 開発環境

## 必要なもの

- Python 3.12以降
- Rust（stable）。Rustのkernelを作るのに使います
- Git
- ローカルWeb画面を使う場合はbrowser
- Node.jsは任意。JavaScript構文をローカル確認するときと、WASMの中核を確かめるときだけ必要

repository rootからpackage、test、lint用toolを導入します。

~~~shell
python -m venv .venv
source .venv/bin/activate
python -m pip install -e ".[dev]"
~~~

Windows PowerShellの場合:

~~~powershell
py -3.12 -m venv .venv
.\.venv\Scripts\Activate.ps1
python -m pip install -e ".[dev]"
$env:SHELTERSIM_LLM_MODE = "offline"
~~~

任意の施設図面画像処理は python -m pip install -e ".[dev,plan-import]"、Claude連携は python -m pip install -e ".[dev,llm]" で追加します。続けて、次の「Rustのkernel」の手順でkernelを導入します。

## Rustのkernel

Rustのkernel（`native/`）は必須です。Rust（stable）とmaturinで作って導入します。導入しないと、`sheltersim`を読み込むときにImportErrorになります。

~~~shell
python -m pip install "maturin>=1.7,<2"
maturin build --release -m native/Cargo.toml --out native-dist
python -m pip install --no-index --find-links native-dist sheltersim-native
python -c "import sheltersim.native"
~~~

kernelの構成と確認の道具は[native/README.md](../native/README.md)と[rust-port.md](rust-port.md)にあります。

### ブラウザー向けのWASM（試作）

中核（`sheltersim_core`）は、Pythonで作って書き出した世界のtickを、ブラウザーのWASMで進められます（#132の段階4、#244）。JSとの窓口は`native/wasm/`（`sheltersim_wasm`、wasm-bindgen）です。作るには、wasm32のtargetと、crateと同じ版のwasm-bindgenのCLI（`native/wasm/Cargo.toml`の`wasm-bindgen = "=…"`）が要ります。Node.jsで確かめるときはNode.jsも要ります。

~~~shell
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
cargo build --manifest-path native/Cargo.toml -p sheltersim_wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target nodejs --out-dir wasm-pkg native/target/wasm32-unknown-unknown/release/sheltersim_wasm.wasm
node native/wasm/tests/node_digests.mjs wasm-pkg
~~~

最後のcommandは、`native/core/tests/data/tick_core.json`の世界をWASMで進め、ネイティブの経路と同じdigestになることを確かめます。CIのlintのjobも同じことをします。

ブラウザーの経路（`PlainRun`）は、イベントのログの記録を正準JSONではなく、binaryの形で持ちます（`EventLog::pack_records`、`events::packed`、#252）。記録の`EventRow`と結果を、そのまま詰めた形です。文字列は表の番号にし、「接頭辞＋番号」のIDは接頭辞の番号と数に分けます。正準JSONは、digest、`find_record`、書き出しのときに、ネイティブの経路と同じ書き手で作るので、bytesは同じです。指標の標本も、`MetricSample`の欄をそのまま詰めて持ちます（`plain::MetricSamples`）。数の根拠になったIDの一覧（`source_ids_json`）は、値として詰めて書き戻したときに同じ文字列になる場合だけ詰め、そうでなければ文字列のまま持ちます。7日・100人の生成Scenarioでは、ログは283 MiBのJSONの代わりに41 MiBに、指標は52 MiBの代わりに7.3 MiBになり、tickの後のWASMのメモリは166 MiBでした。digestは、ログ全体のJSONを作らずに、記録を1件ずつ読んで求めます。指標のdigestは、標本を取るたびに進めておきます。ネイティブの経路（PyO3）は、今までどおり記録のbytesを持ちます。

ブラウザーは、Runを計算しながら、そのサイト専用の保存（OPFS）に残します（#252）。WASMは専用のWorkerで動かし（`native/wasm/js/run_worker.mjs`）、Runのフォルダーに追記だけの2つのファイルを書きます。`data.pack`は保存する部分の集まりで、各部分を`CompressionStream("deflate-raw")`で縮めて追記します。`journal.jsonl`は目録で、1行ごとに部分の場所、縮める前のsha256、行のJSONのsha256を書きます。残すのは、0分の世界（書き出し）、ログと指標の表と塊（塊は閉じるたびに書きます）、最後の時点の世界（ログ以外の部分）、終わりの行（分とdigest）です。途中の時点の世界はまだ残しません（#251）。読み戻しは、hashの合わない最後の行と目録にない部分を捨て、各部分のsha256を確かめてから、中核の読み手（`sheltersim_core::archive`）でログ、指標、世界のdigestを求め、終わりの行と比べます。形の詳しい説明は`native/wasm/js/run_store.mjs`の冒頭にあります。上のNodeのテスト（`node_digests.mjs`）は、メモリーの置き場に書いて読み戻すことも確かめます。

ブラウザーでの時間とメモリは、試しの世界（Projectの画面の生成Scenario、1日、seed 2026、20人と100人）で計ります。`benchmarks/wasm_trial_worlds.py`が0分の世界を書き出し、ネイティブの経路のdigestと時間を記録します。`benchmarks/wasm_trial_measure.py`は、計測のページ（`benchmarks/wasm_trial/index.html`）をPlaywrightのChromiumとFirefoxで開き、WASMで同じ世界を進めます。Playwrightとそのbrowserはprojectの依存ではないので、計る環境に入れます。書き出しと結果は生成物なので、リポジトリーの外に置きます。

~~~shell
python benchmarks/wasm_trial_worlds.py --output wasm-trial
wasm-bindgen --target web --out-dir wasm-web native/target/wasm32-unknown-unknown/release/sheltersim_wasm.wasm
python benchmarks/wasm_trial_measure.py --worlds wasm-trial --pkg wasm-web --output wasm-trial/browser.json
python benchmarks/wasm_trial_measure.py --worlds wasm-trial --pkg wasm-web --store --output wasm-trial/store.json
~~~

`--store`は、計測のページの代わりに保存のページ（`benchmarks/wasm_trial/store.html`）を開きます。世界ごとに1回、WorkerのWASMで進めながらOPFSに残し、読み戻してdigestを確かめ、時間（tick、圧縮、書き込み、読み戻し）、WASMのメモリ、保存の大きさを書きます。

## 設定

packageは環境変数から設定を読み込み、.envを自動読込しません。.env.exampleは変数名と安全な例の一覧です。実keyはshellのsecret managerまたはローカル環境に置き、commitしないでください。

testではoffline modeを明示します。

~~~shell
export SHELTERSIM_LLM_MODE=offline
~~~

意図してAPIを使う場合だけANTHROPIC_API_KEYとSHELTERSIM_LLM_MODE=autoを設定します。routerは設定されたrequest数、同時実行数、token数、費用上限を適用します。live Providerの前にllm-integration.mdを確認してください。

## よく使うcommand

~~~shell
sheltersim --help
sheltersim model-catalog --scenario fixtures/scenarios/h00_small_replay.json
sheltersim prepare --scenario fixtures/scenarios/h00_small_replay.json --freeze --output /tmp/approved.json
sheltersim run --offline --scenario /tmp/approved.json --output /tmp/sheltersim-run
sheltersim inspect --run /tmp/sheltersim-run --events
sheltersim compare --help
sheltersim project --help
~~~

Project CLIでは、Project作成、確認済みScenario保存、Run作成、Branch再開・分岐、履歴検索、利用記録確認ができます。Project dataは既定でruns/.projectsに保存します。CLIの--rootで別のローカルdirectoryを指定できます。

## ローカルWebアプリ

offline workspaceとInspectorの起動方法はdemo-guide.mdを参照してください。serverは既定で127.0.0.1にbindします。dataは設定されたRun/Project rootの下にローカル保存します。認証がないため、hostedな複数利用者向けサービスではありません。

## packageと任意依存

pyproject.tomlでcoreと開発依存を固定しています。Anthropic SDKと施設図面画像/PDF helperは任意依存です。PDF.jsはsrc/sheltersim/web/static/vendor/pdfjsに同梱され、licenseとnoticeを保持しています。
