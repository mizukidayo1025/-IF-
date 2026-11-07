# ローカルデモと管理画面

審査員などに共有のVMで使ってもらう手順は[vm-deploy.md](vm-deploy.md)にあります。

## アプリの起動

開発依存を入れ、repository rootで次を実行します。

~~~shell
python -m pip install -e ".[dev]"
SHELTERSIM_LLM_MODE=offline sheltersim web \
  --runs-dir fixtures/demo_replay/runs \
  --demo-scenario fixtures/scenarios/baseline_100_7d.json \
  --host 127.0.0.1 --port 8765
~~~

次のURLを開きます。

- http://127.0.0.1:8765/demo: Project workspaceとoffline demo。
- http://127.0.0.1:8765/demo-v2: 独立した4画面の「避難所IF！」バックアップUI（[操作・API対応・検証](ui-backup.md)）。
- http://127.0.0.1:8765/admin: 保存Runの参照画面。

Project workspaceの計算と分岐は、Runごとに別プロセスで動きます。異なるRunは、既定で2つまで同時に計算できます。`--max-parallel-runs N` で変えられます。保存データ（Run、Checkpoint、履歴の索引）は、`--runs-dir`の`.projects`ごとに既定で10 GB（10^9バイト）までです。`--project-storage-gb N`で変えられます。計算1本で、1日分のRun（100人）なら約0.5〜0.9 GBのメモリを使うので、PCのメモリに合わせて決めてください。

このserverはローカル用途で、loginを持ちません。別途確認済みのaccess controlがない限りloopbackにbindしてください。

## デモデータ

repositoryに含むfixtures/demo_replayには、合成baselineと介入比較の事前計算結果があります。browserと回帰testが直接参照するため保持しています。このreplayはlive simulationでも、実際の避難所記録や予測でもありません。

同梱Scenarioも合成データです。Providerを設定していない場合、free-text previewはoffline parserを使います。previewだけでBranchは変更されません。介入は確認と明示承認の後に子Branchを作ります。

## Runの確認

admin画面は保存済みRun directoryを読みます。小さなRunは次のように作成できます。

~~~shell
SHELTERSIM_LLM_MODE=offline sheltersim prepare \
  --scenario fixtures/scenarios/h00_small_replay.json \
  --freeze --output /tmp/sheltersim-approved.json
SHELTERSIM_LLM_MODE=offline sheltersim run --offline \
  --scenario /tmp/sheltersim-approved.json \
  --output /tmp/sheltersim-run
~~~

CLIでRunを確認するか、sheltersim web --runs-dir /tmpでローカルdirectoryを表示できます。保存Runの参照はread-onlyです。Project workspaceは永続ローカルstoreと、状態変更用の別APIを使います。

## 事前計算replayの再作成

同梱replayを再作成すると長時間のoffline simulationを実行し、生成ファイルを上書きします。デモ利用には不要です。

~~~shell
SHELTERSIM_LLM_MODE=offline python scripts/build_demo_replay.py --force
~~~

固定結果は決定性回帰確認に使います。合成データを保ち、個人情報や非公開運用データで置き換えないでください。

## 計算済みの例を新しい画面で開く

新しい画面（`/demo-v2`）はProjectのRunを読むため、上の事前計算replayは開けません。同じ種類の比較（7日間と、4日目14:00にトイレ1基が故障する分岐）を、新しい画面が地図を描けるProjectとして作るには、`scripts/build_project_sample.py`を使います。出力先は、`sheltersim web`の`--runs-dir`の下の`.projects`です。

~~~shell
SHELTERSIM_LLM_MODE=offline python scripts/build_project_sample.py /tmp/sheltersim-sample/runs/.projects
SHELTERSIM_LLM_MODE=offline sheltersim web --runs-dir /tmp/sheltersim-sample/runs \
  --host 127.0.0.1 --port 8765
~~~

- Scenarioは、画面の「例文を使う」が入れる文章（「墨田区で地震が発生。避難者100人、7日間。高齢者や子ども連れの支援ニーズも見たい。」）を、画面と同じ`scenario-preview`（offline parser）と`scenario-approve`に通して作ります。標準の合成施設（トイレ`toilet_01`〜`toilet_03`と`accessible_toilet_01`など）、100人、seed 2026です。地域の公開データの調べ（国土地理院・e-Statなどへの接続）は、結果が日によって変わるため行いません。Scenarioの記録（`provenance.region_research`）に、その旨と固定の日付が残ります。
- その後、`main`の最後までの計算、4日目14:00の「Day 4 14:00 toilet_01 outage」のoffline parserでの介入プレビューと明示承認、分岐の最後までの計算を、新しい画面と同じProjectAPIの操作で順に行います。Providerは呼ばず、利用台帳と応答テープは一時directoryに置きます。そのProjectがすでにある出力先には作りません。
- 終わると、Scenario（文章、ID、hash、施設、トイレ）、Run ID、分岐、各分岐のworld・イベント・指標のhash、大きさを表示し、出力先の隣の`sample-build.json`（例では`/tmp/sheltersim-sample/runs/sample-build.json`）にも書きます。同じcommitなら、作り直してもhashは同じです。
- 表示される`open_path`を開きます。例：http://127.0.0.1:8765/demo-v2?project=sample&run=run-sample-toilet-outage&branch=main#simulation 。元のシナリオの1日目00:00から地図の上で再生でき、分岐比較の画面で「比較を更新」を押すとトイレが故障した分岐（`branch-toilet01-outage`）と比べられます。分岐は、シミュレーションの画面の分岐の選択でも開けます。
- 計算済みの合成シナリオの例で、実際の避難所の記録や予測ではありません。1つのseedの比較は記述であり、介入の効果の根拠にはなりません。共有のVMでの使い方は[vm-deploy.md](vm-deploy.md)の「計算済みの例」にあります。

## 困ったとき

- 比較が表示されない場合、--runs-dirがfixtures/demo_replay/runsを指し、fixtures/demo_replay/comparison.jsonが存在するか確認します。
- 8765番portが使用中なら別portを指定し、そのURLを開きます。
- server再起動でprocess内Demo Branchは初期化されます。永続Projectと保存Runは設定されたProjectStore directoryを使います。
- 静的appは手動のページ描画用にPDF.jsを同梱します。OCRや施設geometryの正確な抽出を保証しません。
