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

ある分の地図と指標（frame、APIの`TimelineFrame`）は、中核が作ります（`sheltersim_core::frame`、#264）。WASMでは`Simulation.frame(runId, branchId, computedUntil, stateHash)`が、frameの正準JSONのbytesを返します。`stateHash`を渡さないときは、世界のdigest（`Simulation.stateDigest()`、Pythonの`world.state_digest()`と同じ値）を入れます。digestは、ログを1件ずつhashに流して求め、世界全体のJSONは作りません。ログは追記だけなので、Run（`PlainRun`）はログの128件ごとの塊のhashと途中の塊のhashの状態を持ち続け、次のdigestでは増えた記録だけを流します（`canonical::state_digest_with`、#266）。最初のdigest（作った直後や途中の点から戻した直後）だけがログ全体を読みます。値は変わりません。計った時間は#266に記録しています。`node native/wasm/tests/node_frames.mjs wasm-pkg`は、`native/core/tests/data/frames.json`の世界と分で、WASMのframeがネイティブの経路のframeと同じbytesになることを確かめます。CIのlintのjobも実行します。

ブラウザーの経路（`PlainRun`）は、イベントのログの記録を正準JSONではなく、binaryの形で持ちます（`EventLog::pack_records`、`events::packed`、#252）。記録の`EventRow`と結果を、そのまま詰めた形です。文字列は表の番号にし、「接頭辞＋番号」のIDは接頭辞の番号と数に分けます。正準JSONは、digest、`find_record`、書き出しのときに、ネイティブの経路と同じ書き手で作るので、bytesは同じです。指標の標本も、`MetricSample`の欄をそのまま詰めて持ちます（`plain::MetricSamples`）。数の根拠になったIDの一覧（`source_ids_json`）は、値として詰めて書き戻したときに同じ文字列になる場合だけ詰め、そうでなければ文字列のまま持ちます。計ったメモリと保存の大きさは#132と#138に記録しています。digestは、ログ全体のJSONを作らずに、記録を1件ずつ読んで求めます。指標のdigestは、標本を取るたびに進めておきます。ネイティブの経路（PyO3）は、今までどおり記録のbytesを持ちます。

ブラウザーは、Runを計算しながら、そのサイト専用の保存（OPFS）に残します（#252）。WASMは専用のWorkerで動かし（`native/wasm/js/run_worker.mjs`）、Runのフォルダーに追記だけの2つのファイルを書きます。`data.pack`は保存する部分の集まりで、各部分を`CompressionStream("deflate-raw")`で縮めて追記します。`journal.jsonl`は目録で、1行ごとに部分の場所、縮める前のsha256、行のJSONのsha256を書きます。残すのは、0分の世界（書き出し）、ログと指標の表と塊（塊は閉じるたびに書きます）、途中の点（下の段落、6時間ごとと最後）、最後の時点の世界（ログ以外の部分）、終わりの行（分とdigest）です。読み戻しは、hashの合わない最後の行と目録にない部分を捨て、各部分のsha256を確かめてから、中核の読み手（`sheltersim_core::archive`）でログ、指標、世界のdigestを求め、終わりの行と比べます。形の詳しい説明は`native/wasm/js/run_store.mjs`の冒頭にあります。上のNodeのテスト（`node_digests.mjs`）は、メモリーの置き場に書いて読み戻すことも確かめます。

中核は、ブラウザーのRunを途中の分から計算し直せるよう、その分の状態を残し（`PlainRun::saved_point`）、そこから戻せます（`archive::Restore`、`PlainRun::restore`、#251）。残す点（`archive::SavedPoint`）は、版つきのbinaryで、ログ以外の世界（`archive::world_parts`）と、その分までのログの記録と指標の標本の数、契約の数え（`PlainHost::contracts`）を持ちます。乱数は世界のseedから決まり、IDの採番・予定イベント・モデルの状態は世界の部分にあります。図面は世界のoverlayから作り直し、活動中の行動はログから作り直します（Pythonの`active_action_index`と同じ）。ログと標本は写しません。戻すときは、保存したログと標本の表と塊を順に読み、残す点の数までを取ります。PythonのCheckpointとは互換にしません（#263）。別の版の残す点は読まず、誤りにします。`native/core/tests/archive_store.rs`が、1日のRunの6時間ごとの点に戻して同じdigestになること、途中の点から終わりまで進めて同じdigestになることを確かめます。

ブラウザーのRunは、途中の点を`saved`の部分として、既定で6時間（360分）ごとと最後に残します（`runAndStore`の`saveEvery`、0で残さない。#263）。目録の行は、点の分、記録と標本の数、点の版、その記録と標本を持つ塊の数を持ちます。分へ飛ぶときは（`RunReader.open`のあと`seek(minute)`）、その分以前で一番近い点に戻し、保存したログと標本を点の数まで読んでから、残りの分（最大359 tick）を計算します。点がなければ0分の世界から計算します。この形は`sheltersim-browser-run-v3`です（#266）。v3では、資源の台帳のある世界の指標の標本が、その時点の台帳の指標の列（水と食事の初期量・配送・提供・損失・残り・予約。ネイティブの経路の`resource_snapshots`）も持ちます。台帳の列を持つ標本は1 byte目が1で、持たない標本は前と同じ形です（1 byte目は分のzigzagで偶数）。v3の前の`v2`（#251）と、途中の点のない`v1`（#252）のRunも読み、v1で飛ぶときは0分から計算します。ほかの形と別の版の点は誤りにします。Nodeのテスト`native/wasm/tests/node_saved_points.mjs`は、メモリーの置き場で、各点、点の間、最初の点の前、終わりの分へ飛んで、止めずに進めたRunと同じdigestになることを確かめます。100人・7日で計った大きさと時間は#251に記録しています。

ブラウザーでの時間とメモリは、試しの世界（Projectの画面の生成Scenario、1日、seed 2026、20人と100人）で計ります。`benchmarks/wasm_trial_worlds.py`が0分の世界を書き出し、ネイティブの経路のdigestと時間を記録します。`benchmarks/wasm_trial_measure.py`は、計測のページ（`benchmarks/wasm_trial/index.html`）をPlaywrightのChromiumとFirefoxで開き、WASMで同じ世界を進めます。Playwrightとそのbrowserはprojectの依存ではないので、計る環境に入れます。書き出しと結果は生成物なので、リポジトリーの外に置きます。

~~~shell
python benchmarks/wasm_trial_worlds.py --output wasm-trial
wasm-bindgen --target web --out-dir wasm-web native/target/wasm32-unknown-unknown/release/sheltersim_wasm.wasm
python benchmarks/wasm_trial_measure.py --worlds wasm-trial --pkg wasm-web --output wasm-trial/browser.json
python benchmarks/wasm_trial_measure.py --worlds wasm-trial --pkg wasm-web --store --output wasm-trial/store.json
~~~

`wasm_trial_worlds.py`の`--minutes`は、Scenarioの長さを変えます（既定は1日。7日なら`--minutes 10080`。世界の名前は`trial-100-7d`のようになります）。7日分はProjectの経路の計算に時間がかかるので、要らなければ`--skip-project`で省きます。

`--store`は、計測のページの代わりに保存のページ（`benchmarks/wasm_trial/store.html`）を開きます。世界ごとに1回、WorkerのWASMで進めながらOPFSに残し、読み戻してdigestを確かめ、時間（tick、圧縮、書き込み、読み戻し）、WASMのメモリ、保存の大きさを書きます。途中の点は`--save-every`分ごとに残し（既定360）、点から一番遠い分へ飛ぶ時間（戻す、計算する）と、点の大きさと残す時間も書きます。`--chain`を付けると、各点に戻して次の点まで計算し、残した点と同じになるかも確かめます。

ブラウザーは、Project・Run・Branchの記録を、各Branchの保存したRunの隣に置き（`native/wasm/js/project_store.mjs`、#265）、Projectを1つのファイル（`sheltersim-project-bundle`の版1、拡張子`.shelterproj`）に書き出し、読み込みます。記録の形とファイルの形は[project-persistence.md](project-persistence.md#ブラウザーのproject記録と1つのファイル265)にあります。`native/wasm/tests/node_project_store.mjs`は、メモリーの置き場で、記録を作り、書き出して空の置き場に読み込むと答えとdigestが同じになること、壊れたファイルを断ることを確かめます。CIのlintのjobも動かします。

~~~shell
node native/wasm/tests/node_project_store.mjs wasm-pkg
~~~

Branchの指標の時系列（`GET …/history?kind=kpis`）、レポート（`GET …/report`）、LLMの状態（`GET …/llm-status`）には、`native/wasm/js/api_answers.mjs`が、同じRunにPythonのserverが返すJSONと同じbytesで答えます（#266）。時系列は保存した指標の標本から作り（中核の`sheltersim_core::history`、WASMの`KpiHistory`）、Pythonの履歴の索引と同じ行、同じページ送り、同じ誤りにします。小数は中核が書くので、JSで読み直しません。レポートは、画面のofflineのRunが終わったときにserverが書くもの（評価の規則のfallback）を、保存したRunの最後の分から作ります。Pythonと違う所は[project-persistence.md](project-persistence.md#ブラウザーの時系列レポートllmの状態266)にあります。`native/wasm/tests/node_api_answers.mjs`は、`native/core/tests/data/api_answers.json`（`tests/support/core_api_answers.py`が、Projectの画面の経路で計算したRunとserverの答えから作ります）と同じRunをWASMで計算し、いくつもの`start`・`end`・`step`・`limit`とcursorのページ送り、誤りの答えが、serverの答えと同じsha256になることを確かめます。CIのlintのjobも動かします。2番目の引数に、同じ作り方の大きいRunのファイルを渡せます。

~~~shell
node native/wasm/tests/node_api_answers.mjs wasm-pkg
~~~

計算済みの例をブラウザーの形で作るには、`scripts/build_browser_sample.py`を使います。VMの計算済みの例（`scripts/build_project_sample.py`）のmainと同じScenario（例文、100人、7日、seed 2026）を、Pythonがofflineの読み取りで作り、記録とevidenceと0分の世界を書き出します。続けてNodeが同じWASMと`run_store.mjs`で7日を計算し、記録を作って1つのファイルにします（`native/wasm/tools/build_project_bundle.mjs`）。ファイルは生成物なので、リポジトリーの外に置きます。`--inputs`を付けると、Pythonの書き出しをそのディレクトリーに残します。

~~~shell
SHELTERSIM_LLM_MODE=offline python scripts/build_browser_sample.py sample.shelterproj --pkg wasm-pkg
~~~

ブラウザーでの書き出しと読み込みは、`benchmarks/wasm_trial/project.html`で確かめます。`--project`は、試しの世界ごとにOPFSのProjectへ計算し、書き出し、OPFSを消してから読み込み、digestとProjectの道の答えが同じかを確かめます。`--bundle`は、道具が作ったファイルを空のOPFSに読み込み、各Branchのdigestを確かめます。

~~~shell
python benchmarks/wasm_trial_measure.py --worlds wasm-trial --pkg wasm-web --project
python benchmarks/wasm_trial_measure.py --bundle sample.shelterproj --pkg wasm-web
~~~

### ブラウザー版のAPI（#266）

ブラウザー版では、画面（`/demo-v2`、`demo-v2*.js`）を変えずに、同じAPIにページの中で答えます（#263）。ブラウザー版の`demo-v2.html`は、画面のscriptより先に`native/wasm/js/api_fetch.js`を読みます。このscriptは`window.fetch`を包み、そのサイトの`/api/v1/...`への要求（画面の`api()`とPDFの`fetch`）を専用のWorker（`native/wasm/js/api_worker.mjs`）へ渡します。ほかの要求は元の`fetch`へ渡します。Workerは、サイト専用の保存（OPFSの`projects/`）とWASMから、Pythonのserverと同じstatusとJSON（誤りは`{"error", "code"}`）で答えます（`native/wasm/js/browser_api.mjs`）。要求は1つずつ順に答え、どの要求もWeb Lock（`sheltersim-browser-store`）の中で答えるので、2つのタブが同じファイルを同時に開くことはありません。

| 道（`/api/v1/projects`から） | ブラウザー版の答え |
|---|---|
| `GET`（一覧）、`POST`（作る）、`GET {project}`、`GET runs`、`GET runs/{run}`、`GET runs/{run}/evidence` | OPFSの記録（`project_store.mjs`の答えの関数、#265） |
| `GET …/branches/{branch}/timeline?minute=` | その分のframe（中核、#264）。`state_hash`は世界のdigest |
| `GET runs/{run}/replay-frames`（1つの分岐） | 1〜8分のframe |
| `GET …/history?kind=kpis`、`report`、`llm-status` | `native/wasm/js/api_answers.mjs`（上の段落）。`history`の`kpis`以外の`kind`は「未対応」 |
| `POST runs/{run}/pdf-report`、`scenario-*`、`resume`、`cancel`、`compare`、2つの分岐の`replay-frames`、`intervention-*`、`questions`、`/api/v1/projects`以外の道 | 409 `{"error": "ブラウザー版では未対応です（…）", "code": "browser_unsupported"}` |

分ごとのframeは、分岐ごとに1つ手元に置いたSimulationから作ります。後の分は、手元のSimulationを進めるほうが途中の点から戻すより安いとき（手元の分がその分以前の一番近い点以後か、差が360分以内）は進め、それ以外は`RunReader.seek`で途中の点に戻してから計算します（#251）。再生（`replay-frames`）は手元のSimulationを進めるだけで、戻しません。手元に置くSimulationは2つまでで、置き換えるものは先に解放します（WASMのメモリーは縮まないため）。digestは、Runが持つログのhashの途中の状態に増えた記録だけを足して求めます（上の段落）。

初めて開いたときは、サイトの計算済みの例（`scripts/build_browser_sample.py`の1つのファイルを分けたもの。目録`sample.json`の形は`browser_api.mjs`の`SAMPLE_FORMAT`）を取り寄せ、各部分と全体のsha256を確かめてから読み込みます（`importProject`）。その間は、画面のお知らせ欄（`#notice`）に進み具合を出します。読み込んだ例のsha256を`projects/`の隣（`site-sample.json`）に残し、サイトの例が変わったときだけ読み込み直します。`native/wasm/tests/node_browser_api.mjs`は、メモリーの置き場で、記録の答え、保存した点・その間・前に戻る分のtimeline、続く再生の窓のframeが止めずに進めたRunのframeと同じbytesになること（再生の窓では戻さないこと）、Pythonと同じ誤り、未対応の道、`api_answers.mjs`の道をつないだこと、例の部分の確かめと読み込みを確かめます。CIのlintのjobも動かします。

~~~shell
node native/wasm/tests/node_browser_api.mjs wasm-pkg
~~~

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
