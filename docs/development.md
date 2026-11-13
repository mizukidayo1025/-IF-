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

介入の文の読み取りに渡す世界の要約（`ScenarioSummary`、#302）も中核が作ります（`sheltersim_core::summary`）。WASMでは`Simulation.scenarioSummary()`が、Pythonの`scenario_summary_from_world(world).model_dump(mode="json")`の正準JSONと同じbytesを返します。serverの`intervention-preview`と同じく、既定のzoneはzoneが1つの世界のそのzone、開始日は`null`です。世界は変えません。`node native/wasm/tests/node_summaries.mjs wasm-pkg`が、`native/core/tests/data/summaries.json`の世界と分で確かめます。CIのlintのjobも実行します。
介入のEffectProgram（#304）は、`effectProgramDump(bytes)`がPythonの`EffectProgram.model_validate(json).model_dump(mode="json")`の正準JSONを返し（そのsha256が承認のhash`approved_program_hash`）、`Simulation.validateEffectProgram(bytes)`が今の世界での`validate_effect_program`の問題をJSONの文字列の並びで返します（`sheltersim_core::program`）。文から読み取る6つの作用（`facility_outage`、`add_staff`、`add_toilet`、`water_outage`、`add_evacuees`、`depart_evacuees`）だけを確かめ、文はPythonと同じです。契約が断るprogramの誤りの文は中核のもので、pydanticの文とは違います。世界の検証（E6、#250）はしません。追加するトイレの置き場所は、世界の写しの上で確かめ、世界は変えません。`Simulation.applyEffectProgram(bytes)`は、分岐の子でPythonの`BranchRuntime.apply_effect_program`がするとおりに、今の世界へprogramを予定に入れます（`schedule_effect_program`、#305）。記録とイベントは全部書くか何も書かず、Pythonが`command_log`に足す項目（`{"command": "effect_program", "program_id", "program_hash", "event_ids", "minute"}`）をJSONで返します。問題があるprogramは、Pythonと同じ文（`EffectProgram could not be scheduled: …`）の誤りにし、世界は変えません。`node native/wasm/tests/node_programs.mjs wasm-pkg`が確かめます。

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

APIキーなしの介入の読み取り（画面の「ここから別の『もしも』を試す」の文、#303）は、中核の`sheltersim_core::interpret`（WASMの`compileInterventionOffline`）が、Pythonの`compile_natural_language`がAPIキーなしで返す`CompilerResult`と同じbytesで答えます。規則の読み取り、節の網羅、6つの作用の検証、結果の文、usageのhashを移しました。正規表現はPythonの書き方のまま、Pythonの`\w`・`\b`・`\d`・`\s`の意味で動かします（`native/core/src/interpret/text.rs`）。primitiveの説明とschema、10進の数字の表はPythonが書き（`native/core/tools/interpret_tables.py`）、pytestが古くないか確かめます。`native/wasm/tests/node_interpret.mjs`が、`native/core/tests/data/offline_interpreter.json`の文と要約の組で、Pythonの答えと同じsha256になることを確かめます。CIのlintのjobも動かします。

~~~shell
node native/wasm/tests/node_interpret.mjs wasm-pkg
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

ブラウザー版では、画面（`/demo-v2`、`demo-v2*.js`）を変えずに、同じAPIにページの中で答えます（#263）。ブラウザー版の`demo-v2.html`は、画面のscriptより先に`native/wasm/js/api_fetch.js`を読みます。このscriptは`window.fetch`を包み、そのサイトの`/api/v1/...`への要求（画面の`api()`とPDFの`fetch`）を専用のWorker（`native/wasm/js/api_worker.mjs`）へ渡します。ほかの要求は元の`fetch`へ渡します。Workerは、サイト専用の保存（OPFSの`projects/`）とWASMから、Pythonのserverと同じstatusとJSON（誤りは`{"error", "code"}`）で答えます（`native/wasm/js/browser_api.mjs`）。要求と計算のひと区切り（下）は1つずつ順に行い、どれもWeb Lock（`sheltersim-browser-store`）の中で行うので、2つのタブが同じファイルを同時に開くことはありません。

| 道（`/api/v1/projects`から） | ブラウザー版の答え |
|---|---|
| `GET`（一覧）、`POST`（作る）、`GET {project}`、`GET runs`、`GET runs/{run}`、`GET runs/{run}/evidence` | OPFSの記録（`project_store.mjs`の答えの関数、#265） |
| `GET …/branches/{branch}/timeline?minute=` | その分のframe（中核、#264）。`state_hash`は世界のdigest |
| `GET runs/{run}/replay-frames`（1つの分岐） | 1〜8分のframe |
| `GET …/history?kind=kpis`、`report`、`llm-status` | `native/wasm/js/api_answers.mjs`（上の段落）。`history`の`kpis`以外の`kind`は「未対応」 |
| `POST runs/{run}/resume`、`POST runs/{run}/cancel` | Workerで計算する・止める（`native/wasm/js/compute.mjs`、#267、下） |
| `POST runs/{run}/pdf-report`、`scenario-*`、`compare`、2つの分岐の`replay-frames`、`intervention-*`、`questions`、`/api/v1/projects`以外の道 | 409 `{"error": "ブラウザー版では未対応です（…）", "code": "browser_unsupported"}` |

分ごとのframeは、分岐ごとに1つ手元に置いたSimulationから作ります。後の分は、手元のSimulationを進めるほうが途中の点から戻すより安いとき（手元の分がその分以前の一番近い点以後か、差が360分以内）は進め、それ以外は`RunReader.seek`で途中の点に戻してから計算します（#251）。再生（`replay-frames`）は手元のSimulationを進めるだけで、戻しません。手元に置くSimulationは2つまでで、置き換えるものは先に解放します（WASMのメモリーは縮まないため）。digestは、Runが持つログのhashの途中の状態に増えた記録だけを足して求めます（上の段落）。

初めて開いたときは、サイトの計算済みの例（`scripts/build_browser_sample.py`の1つのファイルを分けたもの。目録`sample.json`の形は`browser_api.mjs`の`SAMPLE_FORMAT`）を取り寄せ、各部分と全体のsha256を確かめてから読み込みます（`importProject`）。その間は、画面のお知らせ欄（`#notice`）に進み具合を出します。読み込んだ例のsha256を`projects/`の隣（`site-sample.json`）に残し、サイトの例が変わったときだけ読み込み直します。`native/wasm/tests/node_browser_api.mjs`は、メモリーの置き場で、記録の答え、保存した点・その間・前に戻る分のtimeline、続く再生の窓のframeが止めずに進めたRunのframeと同じbytesになること（再生の窓では戻さないこと）、Pythonと同じ誤り、未対応の道、`api_answers.mjs`の道をつないだこと、例の部分の確かめと読み込みを確かめます。CIのlintのjobも動かします。

~~~shell
node native/wasm/tests/node_browser_api.mjs wasm-pkg
~~~

#### ブラウザーで計算する（#267の段階B）

ブラウザーはScenarioから世界を作りません（#249はまだです）。#263で決めたとおり、Pythonが作った0分の世界から計算します。Workerは例を読み込んだ後、Project `sample`に、例のRunと同じScenarioの、計算していないRun（`run-browser-compute`、`compute.mjs`の`COMPUTE_RUN`）を作ります。記録は例のRunのものにそのRunのIDを付け、状態をPythonの作ったばかりのRunと同じ`queued`にします。保存したRunには、例の保存したRunの`run`の行と`export`の部分（縮めたbytesをそのまま写し、JSONとして読み直しません）だけを置き、mainの分岐は0分まで計算済み（先頭は`export`の行）です。サイトの最初の頁（`/`）は、例のRunと、このRunへのリンクを並べます。最初の頁の「計算をやり直す」は、確認の後、ブラウザーの保存からこのRunのフォルダー（`projects/sample/runs/run-browser-compute`）を消します。Workerは次に画面を開いたとき、計算していないRunを作り直します。例は消しません。画面のタブが開いていて保存のファイルが使われているときは、消さずに、タブを閉じるよう案内します。最初の頁がブラウザーの「戻る」のcache（back-forward cache）から出たときは、読み直します。サイトを上げ直した後に古い頁が残らないようにするためです。画面では、このRunを開き（Runの選択欄にも出ます）、「終了まで計算」（`#compute-end`）か、分を決めて「この時刻まで計算」を押します。

| 道 | 答え |
|---|---|
| `POST runs/{run}/resume` `{branch_id, target_minute}` | Pythonの`submit`と同じ順に確かめ、同じ誤りで断ります（項目の過不足、`measure_performance`が真偽値でない、計算中の分岐、ほかの計算がある（1つずつ）、ないBranch、計算済みの先から終わりまでにない分、ほかのタブが計算中の分岐は`run_worker_busy`）。受けると分岐とRunを`running`にし、`{"branch_id", "run_id", "status": "running", "target_minute"}`を返して、Workerで計算を始めます |
| `POST runs/{run}/cancel` `{}`または`{branch_id}` | そのRunの計算に止まるよう伝え、その時のRunの記録を返します（Pythonと同じく、まだ`running`のことがあります）。何も計算していないのにRunが`running`なら（ほかのタブの計算）、Pythonと同じ`run_worker_busy`で断ります |

計算は、分岐の保存したRunを先頭（下）まで切り戻し、そこへ戻して（`RunReader.seek`、#251。0分は書き出した世界から）、`CHUNK_TICKS`（60）tickずつのひと区切りで進めます。ひと区切りは要求と同じ列に並ぶので、Workerは区切りの間にtimeline、history、記録の要求に答えます。360分ごと（Runの`save_every`）、目標の分、止められた分では、ログと標本の塊を閉じて（`Simulation.seal_blocks`）途中の点を保存し、分岐の記録の`computed_until`と`checkpoint_hash`をそこへ進めます。その分のframeも作って残し、画面が続けて求めるその分のtimelineに使います。終わったら、時計の終わりなら`completed`（保存したRunは`runAndStore`と同じく、最後の点、`world`、`end`の行で終わります）、目標の分か止めたときは`paused`、誤りなら`failed`を分岐とRunの記録に書きます。OPFSのファイルは区切りごとに開いて閉じます（1つのファイルを同時に開けるのは1つのsync access handleなので、区切りの間の読み取りが自分で開けるように）。計算中の分岐はWeb Lock（`sheltersim-compute/{project}/{run}/{branch}`）を持ち続けます。画面は、Runの先頭の分を見ているときだけ時計が先頭に付いて進みます（Pythonの画面と同じ）。

初めて計算を始めるとき、ページは`navigator.storage.persist()`を1回だけ求めます（#265の決定。ページを開いただけでは求めません）。求めたことはlocalStorageに残し、断られても計算は続けます。`persist`はWorkerにはないので、`api_fetch.js`が`resume`の要求を見て呼びます。

計算の途中でページを閉じると、Workerも止まります。分岐の記録は最後に進めた先頭のままで、保存したRunにはその後の行（や、途中で切れた行）が残ることがあります。次に開いたとき、Workerは、`running`のままでWeb Lockを誰も持っていない分岐とRunを`interrupted`にします（Pythonのserverの起動時と同じ）。先頭の分までは読め、「計算」で先頭から続けられます。記録のファイルが書きかけで読めないときは、保存したRunの最後の先頭で書き直します（Runの記録なら、計算するRunを作り直します）。

`native/wasm/tests/node_compute.mjs`は、メモリーの置き場（OPFSと同じく、1つのファイルを同時に1つしか開けない）で、計算するRunの作り方、Pythonと同じ誤りとJSON、区切りの間の答えと先頭のframe、止める、ページを閉じた後の`interrupted`と続き、記録が書きかけのときの直し方を確かめ、こうして何回にも分けて計算したRunの世界・ログ・標本のdigestとframeと時系列が、止めずに計算した例のRunと同じになることを確かめます。CIのlintのjobも動かします。2つ目の引数に例のファイル（`build_browser_sample.py`）を渡すと、100人・7日の例で同じことを確かめます。

~~~shell
node native/wasm/tests/node_compute.mjs wasm-pkg
node native/wasm/tests/node_compute.mjs wasm-pkg sample.shelterproj
~~~

### ブラウザー版の静的サイト（#266）

ブラウザー版は、サーバーなしで動く静的サイトです。画面のファイル（`demo-v2*`）、ブラウザー版の`demo-v2.html`（serverのものに、上のscriptの1行と、従来の画面へのリンクを隠すstyleの1行を足したもの）、`native/wasm/js`のAPIのファイル、WASM（`--target web`）、計算済みの例を分けたものを、`scripts/build_static_site.py`が1つのディレクトリーに組み立てます。サイトは生成物なので、リポジトリーの外に作り、commitしません。

~~~shell
cargo build --manifest-path native/Cargo.toml -p sheltersim_wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target nodejs --out-dir wasm-pkg native/target/wasm32-unknown-unknown/release/sheltersim_wasm.wasm
wasm-bindgen --target web --out-dir wasm-web native/target/wasm32-unknown-unknown/release/sheltersim_wasm.wasm
SHELTERSIM_LLM_MODE=offline python scripts/build_browser_sample.py sample.shelterproj --pkg wasm-pkg
python scripts/build_static_site.py ../hinanjo-if-site --pkg wasm-web --bundle sample.shelterproj
python scripts/serve_static_site.py ../hinanjo-if-site --port 8000
~~~

最後のcommandは、手元で確かめるためのserverです（127.0.0.1だけ）。`http://127.0.0.1:8000/`を開くと、例のRun（`/demo-v2?project=sample&run=run-sample-toilet-outage&branch=main#simulation`）と、ブラウザーで計算するRun（`run=run-browser-compute`）へのリンクが出ます。標準ライブラリーの`http.server`は`/demo-v2`を`demo-v2.html`で答えないので、このscriptを使います。

サイトの中身：

| 道 | ファイル |
|---|---|
| `/` | `index.html`。例のRunと、ブラウザーで計算するRunを開く`/demo-v2?…`へのリンク、計算をやり直すボタン |
| `/demo-v2` | `demo-v2.html` |
| `/demo-v2.css`、`/demo-v2*.js`、`/demo-v2-logo.jpg` | 画面のファイル（変えずに写す） |
| `/LICENSE.txt` | リポジトリーの`LICENSE`（MIT） |
| `/browser/` | `api_fetch.js`、`api_worker.mjs`、それが読むmodule、`pkg/`（WASM） |
| `/sample/sample.json`、`/sample/sample.shelterproj.000`… | 例の目録と、`--part-mib`（既定16 MiB）以下に分けた部分 |
| ないファイルの道 | `404.html`（status 404） |

公開するときは、ディレクトリーをそのまま、ドメインの最上位で配るhostに置きます。画面は`/demo-v2.css`などを最上位からの道で読み、`/demo-v2?…`へ`history.replaceState`するので、`https://example.org/repo/`のような下の道では動きません。

- `/demo-v2`を`demo-v2.html`で答えるhost：Cloudflare Pages、GitHub Pages（独自ドメインか、`<user>.github.io`のサイト）、Netlifyは、拡張子のない道を同じ名前の`.html`で答えます。Cloudflare Pagesには、`wrangler pages deploy <ディレクトリー>`でそのまま置けます（Pages Functionsは使いません）。Pagesは`404.html`がないと、ないファイルの道にも最上位の`index.html`を返すので、サイトには`404.html`を置き、ないファイルは404にします。GitHub PagesがJekyllを通さないよう`.nojekyll`も置きます。
- ほかのserverでは、拡張子のない道を`.html`で答える設定が要ります。例えばnginxなら`try_files $uri $uri.html =404;`です。`.mjs`は`text/javascript`、`.wasm`は`application/wasm`で送ります。
- 1つのファイルは`--part-mib`以下か、それより小さく保ちます（道具は20 MiBを超えると止まります。Cloudflare Pagesの上限は1ファイル25 MiBです）。
- サーバーの機能（認証、PDF、LLMの中継）はありません。画面の「従来のデモ」（`/demo`）はサイトに含めず、ブラウザー版の`demo-v2.html`はそのリンクをstyleで隠します。`vendor/`（PDFの表示に使うpdf.js）も、`/demo-v2`が読まないので含めません。
- 初めて開くと、例のファイル全体を取り寄せてブラウザーのOPFSに保存します（大きさと時間は#266）。OPFSを使えないブラウザー（設定やprivateの窓によっては）では、お知らせ欄に誤りを出します。

サイトが画面の道に正しく答えるかは、`benchmarks/static_site_check.py`で確かめます。サイトを上のserverで配り、PlaywrightのChromiumとFirefoxで、それぞれ空のOPFSから開きます。`/`のリンクから`/demo-v2`を開き、例を読み込み、時刻の帯で分へ飛び、240×で数秒再生し、レポートの頁の時系列のグラフが出ること、ページの誤り・consoleの誤り・お知らせ欄の誤りがないことを確かめます。`--python-projects`に、`scripts/build_project_sample.py`で作ったProjectStore（例のmainと同じScenarioをPythonのProjectの経路で計算したもの）を渡すと、記録、保存した点とその間の分のtimeline、飛んだ後と続く再生の窓、いくつかの`step`とページ送りの時系列、report、llm-statusについて、ブラウザーの答えのJSONとPythonのserverの答え（同じプロセスの`ProjectAPI`）を、値の型まで比べます。違って当然の項目（置き場の大きさ、Branchの`checkpoint_hash`と時系列の`anchor_hash`・cursorの中のanchor、0分の`state_hash`）は除き、除いた項目と理由を結果に書きます。0分の`state_hash`は、PythonのProjectの経路が最初のtickの前の世界を`rng_state`がnullのまま残し、ブラウザーが始める書き出した世界は乱数の初めの状態を持つために違います。世界のほかの部分は同じで、1分からは同じです。PlaywrightとそのbrowserはProjectの依存ではないので、確かめる環境に入れます。計った時間は#266に記録しています。

`--compute pieces full`を付けると、ブラウザーで計算するRunも確かめます（#267）。どちらも空のOPFSから始めます。`pieces`は、「終了まで計算」を押し、1日を過ぎたら止め、続け、3日を過ぎたらページを閉じ、開き直して`interrupted`と先頭の分を確かめ、最後まで続けます。`full`は7日を一度に計算して時間を計ります。計算中に画面の時計が進むこと、計算したRunで分へ飛ぶ・再生する・時系列のグラフが出ること、1分・4321分・10080分のframeとKPIの時系列が例のRunとRunのID以外同じ（同じScenarioとseedから同じ世界になる）ことを確かめます。計った時間は#267に記録しています。

~~~shell
SHELTERSIM_LLM_MODE=offline python scripts/build_project_sample.py sample-projects
python benchmarks/static_site_check.py --site ../hinanjo-if-site --python-projects sample-projects --output site-check.json
python benchmarks/static_site_check.py --site ../hinanjo-if-site --compute pieces full --output compute-check.json
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
