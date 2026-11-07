# jphacks/tkb_2615 のGitHub記録アーカイブ

元の [jphacks/tkb_2615](https://github.com/jphacks/tkb_2615) を残し、読み取りで取得したIssue・PR・会話・履歴を保存しました。これはGitHub上の投稿者・日時・PR状態などをそのまま移植した「完全複製」ではありません。元への書き込みや移管は行っていません。

- Issue **89件**を新リポジトリにも再作成し、**open 15件 / closed 74件**と本文・タイトルを読み戻して照合しました。元のコメントは投稿者・日時付きでIssue本文内に保存しています。
- PR **270件**の本文・会話・詳細・マージ情報と、全Issue・PRのタイムラインを閲覧用ページとJSONで保存しています。ネイティブのPRとしての再作成は行っていません。
- 通常コメント **147件**、レビュー提出 **0件**、行コメント **0件**。取得した原文とAPI応答を保存しています。
- 元の **8ブランチ / 14タグ**とそのコミット履歴は、複製時点の状態を `refs/archive/source/heads/*` と `refs/archive/source/tags/*` に保管しています。複製先の通常ブランチ・タグはその後の作業で更新・整理されています。PRのhead **270参照**も `refs/archive/pull/N/head` に保管し、全292参照のSHA一致を確認しました（[照合記録](git-ref-verification.json)）。
- Actionsは **108実行・333ジョブのメタデータ、333件のログ、142個のArtifact ZIP**を保存しています。一覧144個のうち残る2個はダウンロードAPIが404でした。新リポジトリの実行履歴への移植は行っていません。

## 設定と制限

[設定の比較・未反映項目](settings.md) / [取得できなかったAPIと理由](fetch-errors.json) / [検証・保存件数](manifest.json)

公開メタデータで確認できる主要設定は説明文を除いて一致していますが、現在の接続は設定変更APIを持たず、管理設定の一部も取得できません。**設定の完全複製は未完了**です。Secretsは値の読み戻しができないため別途再設定が必要です。添付画像・動画14件はCONNECT時に403となり、本体を取得できなかったため元URLを保持しています（[接続結果](attachments.json)）。Artifactの未取得2件は[404の記録](actions-artifact-download-status.json)を参照してください。

認証済みGitHub CLIとAPIアクセスが利用できる環境では、公開の基本設定・Topics・ラベル定義・マイルストーン定義の差分を補助スクリプトで確認できます。このスクリプトは本作業では未実行です。

```bash
python github-archive/apply-public-settings.py          # 差分表示のみ
python github-archive/apply-public-settings.py --apply  # 上記の公開設定を複製先に反映
```

## 保存形式

- [元Issue番号と新Issue番号の対応表](issue-mapping.json)
- [取得したAPI応答](../raw) / [JSONのSHA-256](raw-sha256.json)
- [Actions実行一覧・ログ・Artifact](actions.md) / [ログ・ZIPのSHA-256](assets-sha256.json) / [添付ファイルの元URL一覧](attachment-links.json)

Markdown本文内の元の `#番号` や相対リンクは原文のままです。確実な参照先は各ページ先頭の元URLを使用してください。新リポジトリ上のIssue本文では `#番号` を元リポジトリへのリンクに変換しています。GitHubによる削除・編集履歴やAPIが公開しない情報は復元できません。

PRのコードを取得する例（リモートをこの複製先に設定済みの場合）：

```bash
git fetch origin refs/archive/pull/296/head
git show FETCH_HEAD
```

複製時点のブランチ・タグを取得する例：

```bash
git fetch origin refs/archive/source/heads/main
git show FETCH_HEAD
git fetch origin refs/archive/source/tags/develop-final
git show FETCH_HEAD
```

## Issue・PR一覧

| 元の記録 | タイトル | 元の状態 | 新Issue |
|---|---|---|---|
| [PR #1](pulls/1.md) | JPHACKSのREADMEテンプレートを復元 | merged | 記録として保存 |
| [PR #2](pulls/2.md) | ShelterSim-test1の実装・日本語文書を取り込み | merged | 記録として保存 |
| [PR #3](pulls/3.md) | LLMのrule fallbackをRun集計とCLIで可視化する | merged | 記録として保存 |
| [PR #4](pulls/4.md) | API利用時の結果がofflineと同じになるfallbackの原因を修正する | merged | 記録として保存 |
| [PR #5](pulls/5.md) | 初期条件入力のAI呼出し（準備stage）の失敗と利用記録の凍結を修正する | merged | 記録として保存 |
| [PR #6](pulls/6.md) | 研究ベースの心理・社会・情報モデルと内部検証を追加 | merged | 記録として保存 |
| [PR #7](pulls/7.md) | AI利用4場面の通し確認で見つかった、応答が結果に使われない問題を修正する | merged | 記録として保存 |
| [PR #8](pulls/8.md) | 介入指示（NL compile）の応答再利用と、有料連携テストの修復 | merged | 記録として保存 |
| [PR #9](pulls/9.md) | 長いRunでもreport用の呼出し枠を残す（記録Runの再生・分岐の検証を含む） | merged | 記録として保存 |
| [PR #10](pulls/10.md) | 世帯関係の修正で他の承認ブロッカーが消えないようにする（Web Scenarioが開始できない原因の調査を含む） | merged | 記録として保存 |
| [PR #11](pulls/11.md) | 標準設定で作ったScenarioを止めずに開始でき、トイレも使われるようにする | merged | 記録として保存 |
| [PR #12](pulls/12.md) | offlineテストをpytest-xdistで並列実行できるようにする | merged | 記録として保存 |
| [PR #13](pulls/13.md) | docs: 心理状態高止まりの原因調査を記録 | merged | 記録として保存 |
| [PR #14](pulls/14.md) | 生成したScenarioで研究版の心理モデルを使い、旧来モデルの利用を表示する | merged | 記録として保存 |
| [Issue #15](issues/15.md) | Claude API利用時とofflineで結果がほとんど変わらない（研究版心理モデルでの比較） | open | [#1](https://github.com/mizukidayo1025/-IF-/issues/1) |
| [PR #16](pulls/16.md) | 数理モデル監査と睡眠負債対応suite v2 | merged | 記録として保存 |
| [PR #17](pulls/17.md) | 在庫を入力した生成Scenarioが実行中に止まる問題を修正 | merged | 記録として保存 |
| [PR #18](pulls/18.md) | トイレ・給水・配食の処理能力を事前に確認する診断を追加 | merged | 記録として保存 |
| [PR #19](pulls/19.md) | 列が部屋に収まらないときに、先頭のサービスが止まる問題を修正 | merged | 記録として保存 |
| [PR #20](pulls/20.md) | 生成Scenarioの設備・職員・水と食料を、国の目安以上で用意する | merged | 記録として保存 |
| [PR #21](pulls/21.md) | 列の後ろの人が移動している間も、先頭の人がサービスを受けられるようにする | merged | 記録として保存 |
| [Issue #22](issues/22.md) | 水分需要の係数と給水量（ml）の対応を決めたい | open | [#2](https://github.com/mizukidayo1025/-IF-/issues/2) |
| [PR #23](pulls/23.md) | 国土地理院の指定避難所CSVが、最後まで受信しても毎回取得失敗になる問題を修正 | merged | 記録として保存 |
| [Issue #24](issues/24.md) | 自然文の入力から公開APIの引数を決める「橋渡し」にClaudeを使う（設計案） | open | [#3](https://github.com/mizukidayo1025/-IF-/issues/3) |
| [PR #25](pulls/25.md) | Link region water restorations to their outage events | merged | 記録として保存 |
| [PR #26](pulls/26.md) | Let plain resource losses run without operational event state | merged | 記録として保存 |
| [PR #27](pulls/27.md) | Check only the last tick's history records in per-tick validation | merged | 記録として保存 |
| [PR #28](pulls/28.md) | Check the resource ledger's current state instead of replaying it | merged | 記録として保存 |
| [PR #29](pulls/29.md) | 避難所IF！の4画面UIと記録に基づくゲーム風マップを採用 | merged | 記録として保存 |
| [PR #30](pulls/30.md) | 自然言語の介入指示をOpusに届け、fallback理由を表示する | merged | 記録として保存 |
| [PR #31](pulls/31.md) | Make project storage work on Windows | merged | 記録として保存 |
| [PR #32](pulls/32.md) | ハイフン付きの設備IDを件数の負号と誤読しないようにする | merged | 記録として保存 |
| [PR #33](pulls/33.md) | stop設定では介入の変換をルール処理に切り替えず、理由を返して失敗させる | merged | 記録として保存 |
| [PR #34](pulls/34.md) | 作用の説明欄にAIを使わなかった理由を表示する | merged | 記録として保存 |
| [Issue #35](issues/35.md) | 介入の操作ごとの対象候補を整理する（追加先が自明でも聞き返しになる） | closed | [#4](https://github.com/mizukidayo1025/-IF-/issues/4) |
| [PR #36](pulls/36.md) | 保存付き計算の重複検証と計算中のframe再取得をなくす | merged | 記録として保存 |
| [PR #37](pulls/37.md) | 保存済み時刻のframeを計算側でキャッシュし、timeline取得で履歴を復元しない | merged | 記録として保存 |
| [PR #38](pulls/38.md) | 作成直後のCheckpointの再検証と保存済みchunkの機密検査の繰り返しをやめる | merged | 記録として保存 |
| [PR #39](pulls/39.md) | Checkpoint作成直後に同じハッシュを再計算しない | merged | 記録として保存 |
| [PR #40](pulls/40.md) | 計算開始時にhead Checkpointを1回だけ読み込む | merged | 記録として保存 |
| [PR #41](pulls/41.md) | Make the demo-v2 simulation screen easier to read and operate | merged | 記録として保存 |
| [PR #42](pulls/42.md) | 比較・レポート・会話・Q&Aでbranchの全履歴を読み込まない | merged | 記録として保存 |
| [PR #43](pulls/43.md) | tickの各phaseで施設形状の解析を1回にまとめる | merged | 記録として保存 |
| [Issue #44](issues/44.md) | Checkpoint保存を差分化し、後半の時刻ほど保存が遅くなる構造をなくす | closed | [#5](https://github.com/mizukidayo1025/-IF-/issues/5) |
| [Issue #45](issues/45.md) | tick内で時間とともに伸びる処理を減らす | open | [#6](https://github.com/mizukidayo1025/-IF-/issues/6) |
| [Issue #46](issues/46.md) | 保存境界以外の時刻の表示と分岐の作成で、全履歴の復元を減らす | closed | [#7](https://github.com/mizukidayo1025/-IF-/issues/7) |
| [Issue #47](issues/47.md) | 保存時の索引登録と容量チェックの走査コストを減らす | closed | [#8](https://github.com/mizukidayo1025/-IF-/issues/8) |
| [Issue #48](issues/48.md) | 計算workerを別プロセスにし、Web応答との競合をなくして分岐を並列計算する | closed | [#9](https://github.com/mizukidayo1025/-IF-/issues/9) |
| [PR #49](pulls/49.md) | Add the logo, bubble popups and plain-language KPI notes to demo-v2 | merged | 記録として保存 |
| [Issue #50](issues/50.md) | readmeに使う画像たち | open | [#10](https://github.com/mizukidayo1025/-IF-/issues/10) |
| [PR #51](pulls/51.md) | 介入で追加したトイレを図面に配置し、在所者に知らせる | merged | 記録として保存 |
| [PR #52](pulls/52.md) | 追加したトイレが画面データと地図に描かれることをテストする | merged | 記録として保存 |
| [Issue #53](issues/53.md) | テストを体系化する（利用者の経路を通るScenarioの共通化、層の整理、穴の可視化） | closed | [#11](https://github.com/mizukidayo1025/-IF-/issues/11) |
| [PR #54](pulls/54.md) | レポート保存をOpus分析付き日本語PDF出力に変更 | merged | 記録として保存 |
| [Issue #55](issues/55.md) | readmeに使う画像と動画たちpart2 | open | [#12](https://github.com/mizukidayo1025/-IF-/issues/12) |
| [PR #56](pulls/56.md) | main凍結に伴い統合先をdevelopに変更 | merged | 記録として保存 |
| [Issue #57](issues/57.md) | テストの地図を作る（#53 段階1） | closed | [#13](https://github.com/mizukidayo1025/-IF-/issues/13) |
| [Issue #58](issues/58.md) | 利用者の経路を通る共通fixtureを作る（#53 段階2） | closed | [#14](https://github.com/mizukidayo1025/-IF-/issues/14) |
| [Issue #59](issues/59.md) | テストを範囲（単体・統合・e2e）で置き分ける決まりを作る（#53 段階3） | closed | [#15](https://github.com/mizukidayo1025/-IF-/issues/15) |
| [Issue #60](issues/60.md) | 介入ごとに利用者が見る結果のテストを足す（#53 段階4） | closed | [#16](https://github.com/mizukidayo1025/-IF-/issues/16) |
| [Issue #61](issues/61.md) | 出典台帳とコードの根拠IDの整合テストを足す（#53 段階5） | closed | [#17](https://github.com/mizukidayo1025/-IF-/issues/17) |
| [PR #62](pulls/62.md) | テストの地図を作る（#53 段階1） | merged | 記録として保存 |
| [PR #63](pulls/63.md) | 利用者の経路を通る共通fixtureを作る（#53 段階2） | merged | 記録として保存 |
| [PR #64](pulls/64.md) | テストを範囲（単体・統合・e2e）で置き分ける決まりを作る（#53 段階3） | merged | 記録として保存 |
| [Issue #65](issues/65.md) | 断水と職員の不在の介入が、4日未満の生成Scenarioに適用できない | closed | [#18](https://github.com/mizukidayo1025/-IF-/issues/18) |
| [Issue #66](issues/66.md) | 施設・職員・物資の残りの介入に結果テストを足す（#60） | closed | [#19](https://github.com/mizukidayo1025/-IF-/issues/19) |
| [Issue #67](issues/67.md) | 人口が変わる介入に結果テストを足す（#60） | closed | [#20](https://github.com/mizukidayo1025/-IF-/issues/20) |
| [Issue #68](issues/68.md) | 空間・経路を変える介入に結果テストを足す（#60） | closed | [#21](https://github.com/mizukidayo1025/-IF-/issues/21) |
| [Issue #69](issues/69.md) | 環境を変える介入に結果テストを足す（#60） | closed | [#22](https://github.com/mizukidayo1025/-IF-/issues/22) |
| [PR #70](pulls/70.md) | 介入4種で利用者が見る結果が変わることを確かめる（#60） | merged | 記録として保存 |
| [PR #71](pulls/71.md) | コードが参照する根拠IDと出典台帳の整合テストを足す（#53 段階5） | merged | 記録として保存 |
| [Issue #72](issues/72.md) | 職員の担当を変える介入（set_staff_role）が例外で止まる | closed | [#23](https://github.com/mizukidayo1025/-IF-/issues/23) |
| [Issue #74](issues/74.md) | 職員の不在による支援の待ちが、待ちの理由のKPIに数えられない | closed | [#24](https://github.com/mizukidayo1025/-IF-/issues/24) |
| [Issue #75](issues/75.md) | 生成Scenarioの職員は清掃・保守の技能を持たず、その作業指示が必ず却下される | closed | [#25](https://github.com/mizukidayo1025/-IF-/issues/25) |
| [Issue #76](issues/76.md) | 物資の到着を遅らせると、届く時刻にシミュレーションが例外で止まる | closed | [#26](https://github.com/mizukidayo1025/-IF-/issues/26) |
| [PR #77](pulls/77.md) | 施設・職員・物資の残りの介入に結果テストを足す（#66） | merged | 記録として保存 |
| [Issue #78](issues/78.md) | 退所の介入が、行動中の人がいると実行時に失敗し、誰も退所しない | closed | [#27](https://github.com/mizukidayo1025/-IF-/issues/27) |
| [Issue #79](issues/79.md) | 一時外出などの在所の変更が、給水・配食を利用したことのある人には失敗する | closed | [#28](https://github.com/mizukidayo1025/-IF-/issues/28) |
| [Issue #80](issues/80.md) | 仮設スペースを足す介入が、生成Scenarioには適用できない | closed | [#29](https://github.com/mizukidayo1025/-IF-/issues/29) |
| [PR #81](pulls/81.md) | 人口が変わる介入に結果テストを足す（#67） | merged | 記録として保存 |
| [Issue #82](issues/82.md) | 施設の移動（move_facility）が適用時に必ず失敗する | closed | [#30](https://github.com/mizukidayo1025/-IF-/issues/30) |
| [Issue #83](issues/83.md) | 職員の要る施設を別の区画へ移すと、その施設のサービスが止まる | closed | [#31](https://github.com/mizukidayo1025/-IF-/issues/31) |
| [Issue #84](issues/84.md) | 仮設の部屋のベッドへ向かった人が、着いても眠らない | closed | [#32](https://github.com/mizukidayo1025/-IF-/issues/32) |
| [PR #85](pulls/85.md) | 空間・経路を変える介入に結果テストを足す（#68） | merged | 記録として保存 |
| [PR #86](pulls/86.md) | 環境を変える介入に結果テストを足す（#69） | merged | 記録として保存 |
| [Issue #87](issues/87.md) | Project画面の主な流れをHTTPから通すe2eテストを足す | closed | [#33](https://github.com/mizukidayo1025/-IF-/issues/33) |
| [Issue #88](issues/88.md) | 既存テストのScenarioを、利用者の経路を通る共通fixtureに寄せる | closed | [#34](https://github.com/mizukidayo1025/-IF-/issues/34) |
| [PR #89](pulls/89.md) | トイレ追加を本線と比べる結果テストを足す（#60） | merged | 記録として保存 |
| [PR #90](pulls/90.md) | 1日分を実行する遅いテストに long_simulation の印を付ける | merged | 記録として保存 |
| [Issue #91](issues/91.md) | 凍結されたイベントの値を dict と判定できず、4か所の処理が黙って飛ばされている | closed | [#35](https://github.com/mizukidayo1025/-IF-/issues/35) |
| [PR #92](pulls/92.md) | 凍結されたイベントの値で失敗する物資の遅延・退所・施設の移動を直す（#76・#78・#82） | merged | 記録として保存 |
| [PR #93](pulls/93.md) | 凍結されたイベントの値を Mapping として読み、黙って飛ばされていた4つの処理を直す（#91） | merged | 記録として保存 |
| [PR #94](pulls/94.md) | 職員の担当を変える介入（set_staff_role）が例外で止まる不具合を直す（#72） | merged | 記録として保存 |
| [PR #95](pulls/95.md) | 職員の不在による支援の待ちを、「勤務していない資格者がいる」待ちに数える（#74） | merged | 記録として保存 |
| [PR #96](pulls/96.md) | 給水・配食を使った人の一時外出・転送が失敗する不具合を直す（#79） | merged | 記録として保存 |
| [PR #97](pulls/97.md) | 生成Scenarioで仮設スペースを足せるようにする（#80） | merged | 記録として保存 |
| [PR #98](pulls/98.md) | 仮設の部屋のベッドへ向かった人が、着いても眠らない不具合を直す（#84） | merged | 記録として保存 |
| [PR #99](pulls/99.md) | 職員の要る施設を、担当の職員がいない区画へ移す介入を承認前に却下する（#83） | merged | 記録として保存 |
| [PR #100](pulls/100.md) | 施設の認知のテストを共通fixtureに寄せる（#88：test_facility_orientation.py） | merged | 記録として保存 |
| [PR #101](pulls/101.md) | 図面の設定キャッシュのテストを共通fixtureに寄せる（#88：test_geometry_config_cache.py） | merged | 記録として保存 |
| [PR #102](pulls/102.md) | 追加したトイレの描画のテストを共通fixtureに寄せる（#88：test_installed_facility_rendering.py） | merged | 記録として保存 |
| [PR #103](pulls/103.md) | 介入プレビューのテストを共通fixtureに寄せる（#88：test_nl_opus.py） | merged | 記録として保存 |
| [PR #104](pulls/104.md) | 研究版心理モデルの既定のテストを共通fixtureに寄せる（#88：test_research_psychology_default.py） | merged | 記録として保存 |
| [PR #105](pulls/105.md) | 資源の損失のテストを共通fixtureに寄せる（#88：test_resource_loss_without_operational_state.py） | merged | 記録として保存 |
| [PR #106](pulls/106.md) | モデル評価のテストを共通fixtureに寄せる（#88：test_model_evaluation.py） | merged | 記録として保存 |
| [PR #107](pulls/107.md) | 部屋に入りきらない行列のテストを共通fixtureに寄せる（#88：test_physical_queue_overflow.py） | merged | 記録として保存 |
| [PR #108](pulls/108.md) | 行列を詰める間のサービスのテストを共通fixtureに寄せる（#88：test_queue_service_while_repositioning.py） | merged | 記録として保存 |
| [PR #109](pulls/109.md) | 在庫を入力したScenarioのテストを共通fixtureに寄せる（#88：test_resource_queue_regressions.py） | merged | 記録として保存 |
| [PR #110](pulls/110.md) | 世帯の修正のテストを画面と同じプレビューの経路に寄せる（#88：test_household_correction.py） | merged | 記録として保存 |
| [PR #111](pulls/111.md) | 支援の記録のテストを共通fixtureに寄せる（#88：test_care_lifecycle.py） | merged | 記録として保存 |
| [PR #112](pulls/112.md) | 運用条件の断水のテストを共通fixtureに寄せる（#88：test_region_water_outage_conditions.py） | merged | 記録として保存 |
| [PR #113](pulls/113.md) | 処理能力の診断のテストを共通fixtureに寄せる（#88：test_service_capacity.py） | merged | 記録として保存 |
| [PR #114](pulls/114.md) | 標準の設備・物資のテストを共通fixtureに寄せる（#88：test_standard_provisioning.py） | merged | 記録として保存 |
| [Issue #115](issues/115.md) | 開発の回の名前のテストファイル（round2・round3）を担当と範囲ごとに分ける | closed | [#36](https://github.com/mizukidayo1025/-IF-/issues/36) |
| [PR #116](pulls/116.md) | 生成Scenarioを使う1件を test_round2_common_services.py から切り出す（#88） | merged | 記録として保存 |
| [PR #117](pulls/117.md) | 支援の移動のテストを共通fixtureに寄せる（#88：test_staff_support_travel.py） | merged | 記録として保存 |
| [Issue #118](issues/118.md) | 独立した列を持つ給水所で、容量が提供地点の数より小さいと状態の検証で止まる | closed | [#37](https://github.com/mizukidayo1025/-IF-/issues/37) |
| [PR #119](pulls/119.md) | 図面だけを足したfixtureの残り3本を共通fixtureに寄せる（#88） | merged | 記録として保存 |
| [PR #120](pulls/120.md) | 並列実行で、長時間テストを各workerの最初に1本ずつ割り振る | merged | 記録として保存 |
| [PR #121](pulls/121.md) | 開発の回の名前のテストファイル2本を担当と範囲ごとに分ける | merged | 記録として保存 |
| [PR #122](pulls/122.md) | Project画面の主な流れをHTTPから通すe2eテストを足す | merged | 記録として保存 |
| [PR #123](pulls/123.md) | 独立した列を持つ給水所で、容量が提供地点より少なくても止まらないようにする | merged | 記録として保存 |
| [PR #124](pulls/124.md) | 4日未満の生成Scenarioでも断水と職員の不在を適用できるようにする | merged | 記録として保存 |
| [PR #125](pulls/125.md) | 職員が受けられない作業指示を、理由を示して承認前に却下する | merged | 記録として保存 |
| [Issue #126](issues/126.md) | 決まりを作る前のテストファイル82本を、担当と範囲ごとに移す | closed | [#38](https://github.com/mizukidayo1025/-IF-/issues/38) |
| [PR #127](pulls/127.md) | core・events・regionの既存テスト7本を担当と範囲ごとに移す（#126 の1本目） | merged | 記録として保存 |
| [PR #128](pulls/128.md) | domainの既存テスト14本を担当と範囲ごとに移す（#126 の2本目） | merged | 記録として保存 |
| [PR #129](pulls/129.md) | policiesの既存テスト9本を担当と範囲ごとに移す（#126 の3本目） | merged | 記録として保存 |
| [PR #130](pulls/130.md) | experimentsの既存テスト前半8本を担当と範囲ごとに移す（#126 の4本目） | merged | 記録として保存 |
| [PR #131](pulls/131.md) | experimentsの既存テスト後半7本を担当と範囲ごとに移す（#126 の5本目） | merged | 記録として保存 |
| [PR #132](pulls/132.md) | llmの既存テスト11本を担当と範囲ごとに移す（#126 の6本目） | merged | 記録として保存 |
| [PR #133](pulls/133.md) | storage・cliの既存テスト7本を担当と範囲ごとに移す（#126 の7本目） | merged | 記録として保存 |
| [PR #134](pulls/134.md) | webの既存テスト前半10本を担当と範囲ごとに移す（#126 の8本目） | merged | 記録として保存 |
| [PR #135](pulls/135.md) | webの既存テスト後半9本を移し、移行の決まりを整理する（#126 の9本目） | merged | 記録として保存 |
| [Issue #136](issues/136.md) | 全件実行で失敗したテストとその原因を、後から確かめられるようにする | closed | [#39](https://github.com/mizukidayo1025/-IF-/issues/39) |
| [Issue #137](issues/137.md) | fixtureのScenarioで確かめているテストを洗い出し、生成Scenarioへ置き換える | closed | [#40](https://github.com/mizukidayo1025/-IF-/issues/40) |
| [Issue #138](issues/138.md) | 肥大化したtests/conftest.pyを題材ごとに整理する | closed | [#42](https://github.com/mizukidayo1025/-IF-/issues/42) |
| [Issue #139](issues/139.md) | ruffの既存の指摘を一括整形で片付け、lintが通る状態にする | closed | [#43](https://github.com/mizukidayo1025/-IF-/issues/43) |
| [PR #140](pulls/140.md) | ruffの既存の指摘を一括整形で片付け、lintが通る状態にする | merged | 記録として保存 |
| [Issue #141](issues/141.md) | CONTRIBUTING.mdのテスト置き場所の説明を、今の決まりに合わせる | closed | [#44](https://github.com/mizukidayo1025/-IF-/issues/44) |
| [PR #142](pulls/142.md) | CONTRIBUTING.mdのテスト置き場所の説明を、今の決まりに合わせる | merged | 記録として保存 |
| [Issue #143](issues/143.md) | demo jobとProject実行の完了待ちを、実時間の期限つきpollingから完了を待つ形にする | closed | [#45](https://github.com/mizukidayo1025/-IF-/issues/45) |
| [PR #144](pulls/144.md) | 全件実行の結果をファイルに残し、失敗したテストを最後の行に出す | merged | 記録として保存 |
| [Issue #145](issues/145.md) | test_resume_loads_the_head_checkpoint_onceが、背景workerとの競合でときどき失敗する | closed | [#46](https://github.com/mizukidayo1025/-IF-/issues/46) |
| [Issue #146](issues/146.md) | scripts/test-live-api.shが、移動前のパスのライブテストを実行しようとする | closed | [#47](https://github.com/mizukidayo1025/-IF-/issues/47) |
| [PR #147](pulls/147.md) | ライブテストのスクリプトを、移動後のパスで実行する | merged | 記録として保存 |
| [Issue #148](issues/148.md) | ライブテストのモデル別確認が、runtimeの呼び出し枠の規則で必ず失敗する | closed | [#48](https://github.com/mizukidayo1025/-IF-/issues/48) |
| [PR #149](pulls/149.md) | test_resume_loads_the_head_checkpoint_onceの背景workerとの競合をなくす | merged | 記録として保存 |
| [PR #150](pulls/150.md) | demo jobとProject実行の完了を、実時間の期限つきpollingではなく完了そのもので待つ | merged | 記録として保存 |
| [PR #151](pulls/151.md) | ライブテストのモデル別確認を、runtimeの枠の規則と実行ごとの口座に合わせる | merged | 記録として保存 |
| [Issue #152](issues/152.md) | ライブテストの公式ドメイン検索が固定の口座IDを使い、呼び出しの上限がPCで通算になる | closed | [#49](https://github.com/mizukidayo1025/-IF-/issues/49) |
| [PR #153](pulls/153.md) | ライブテストの公式ドメイン検索を、実行ごとの口座で予約する | merged | 記録として保存 |
| [PR #154](pulls/154.md) | 再開・分岐・seek・trace levelのテストを生成Scenarioで確かめる（#137） | merged | 記録として保存 |
| [PR #155](pulls/155.md) | 保存したframeと一括再生のテストを生成Scenarioで確かめる（#137） | merged | 記録として保存 |
| [PR #156](pulls/156.md) | 給水・配食のテストを生成Scenarioで確かめる（#137） | merged | 記録として保存 |
| [Issue #157](issues/157.md) | 行動Policyの比較が、生成Scenarioでは必ず「入力が違う」と拒否される | closed | [#50](https://github.com/mizukidayo1025/-IF-/issues/50) |
| [PR #158](pulls/158.md) | 比較のテストを生成Scenarioと実行結果で確かめる（#137） | merged | 記録として保存 |
| [Issue #159](issues/159.md) | 高い苦痛のモニターが、イベントの処理でしきい値を下回った回復を数えない | closed | [#51](https://github.com/mizukidayo1025/-IF-/issues/51) |
| [Issue #160](issues/160.md) | PDFレポートで、EffectProgramの介入の数量（count・quantity）が落ちる | closed | [#52](https://github.com/mizukidayo1025/-IF-/issues/52) |
| [Issue #161](issues/161.md) | 保存したRunのinspectorの地図が、生成Scenarioの区画と人を描かない | closed | [#53](https://github.com/mizukidayo1025/-IF-/issues/53) |
| [PR #162](pulls/162.md) | 情報拡散・ストレスの伝播・心理の回復を生成Scenarioで確かめる（#137） | merged | 記録として保存 |
| [PR #163](pulls/163.md) | 支援の優先度の再生とPDFレポートの介入を生成Scenarioで確かめる（#137） | merged | 記録として保存 |
| [PR #164](pulls/164.md) | Project画面の介入と行動Policyの選択を生成Scenarioで確かめる（#137） | merged | 記録として保存 |
| [PR #165](pulls/165.md) | tests/conftest.py を題材ごとのモジュール（tests/support/）に分ける（#138） | merged | 記録として保存 |
| [PR #166](pulls/166.md) | 似たfixtureをまとめ、別名fixtureと重複helperをなくす（#138） | merged | 記録として保存 |
| [PR #167](pulls/167.md) | fixtureのパスの定数をsupport.pathsにまとめる（#138） | merged | 記録として保存 |
| [PR #168](pulls/168.md) | 生成Scenarioの行動Policyを比べられるようにする（#157） | merged | 記録として保存 |
| [Issue #169](issues/169.md) | モデル一式だけを指定したScenarioで、manifestに記録したモデルと実行時のモデルが違う | closed | [#54](https://github.com/mizukidayo1025/-IF-/issues/54) |
| [PR #170](pulls/170.md) | モデル一式が個別の指定を省いた領域でも、宣言したモデルで実行する（#169） | merged | 記録として保存 |
| [Issue #171](issues/171.md) | モデル一式の検証結果が、宣言とは違うモデル（queue・relationship・誤情報の圧力）で計算されている | open | [#55](https://github.com/mizukidayo1025/-IF-/issues/55) |
| [PR #172](pulls/172.md) | モデル一式v2の検証を#169の修正後に実行し直し、元の結果に注記する（#171） | merged | 記録として保存 |
| [PR #173](pulls/173.md) | イベントの処理で苦痛がしきい値を下回った回復も、モニターで数える（#159） | merged | 記録として保存 |
| [PR #174](pulls/174.md) | PDFレポートに、EffectProgramの介入の数量を出す（#160） | merged | 記録として保存 |
| [PR #175](pulls/175.md) | 保存したRunの地図を図面から描き、描けない人を明示する（#161） | merged | 記録として保存 |
| [PR #176](pulls/176.md) | 網羅率を必要なときに測れるようにし、現状を記録する | merged | 記録として保存 |
| [PR #177](pulls/177.md) | Project画面の証跡・実験の経路を、生成ScenarioのRunで確かめるテストを足す | merged | 記録として保存 |
| [Issue #178](issues/178.md) | テストが通っていない利用者の経路を埋め、不具合を探す | closed | [#56](https://github.com/mizukidayo1025/-IF-/issues/56) |
| [Issue #179](issues/179.md) | 支援を要請中の人を再会させると、シミュレーションが例外で止まる | closed | [#57](https://github.com/mizukidayo1025/-IF-/issues/57) |
| [Issue #180](issues/180.md) | 帰還先の区画へ移動中の人を数えず、定員を超えてシミュレーションが例外で止まる | closed | [#58](https://github.com/mizukidayo1025/-IF-/issues/58) |
| [Issue #181](issues/181.md) | 一時外出・移送の介入が、計画では通るのに実行時に失敗し、再試行もされない | closed | [#59](https://github.com/mizukidayo1025/-IF-/issues/59) |
| [PR #182](pulls/182.md) | 避難者の移動・再会・帰還を生成Scenarioで確かめ、見つかった不具合をxfailで置く | merged | 記録として保存 |
| [PR #183](pulls/183.md) | 支援の要請の記録を、要請したときの世帯で続ける（#179） | merged | 記録として保存 |
| [PR #184](pulls/184.md) | 区画の受け入れで、そこへ移動中の人を数える（#180） | merged | 記録として保存 |
| [Issue #185](issues/185.md) | 一時外出と帰還を1つの介入に書くと、計画の段階で拒否される | closed | [#60](https://github.com/mizukidayo1025/-IF-/issues/60) |
| [PR #186](pulls/186.md) | 作業中の人の一時外出・移送を、手が空くまで延期する（#181） | merged | 記録として保存 |
| [Issue #187](issues/187.md) | 近接接触のモデルを選ぶと、Checkpointの保存が「機密データを含む」と拒否されて止まる | closed | [#61](https://github.com/mizukidayo1025/-IF-/issues/61) |
| [Issue #188](issues/188.md) | フォームで入れた施設の停止・設備の故障が、その時刻に使用中だと失敗し、最後まで効かない | closed | [#62](https://github.com/mizukidayo1025/-IF-/issues/62) |
| [PR #189](pulls/189.md) | イベントの処理の利用者の経路を生成Scenarioで確かめ、見つかった不具合をxfailで置く | merged | 記録として保存 |
| [Issue #190](issues/190.md) | Runの記録で、モデルの出典のDOIが電話番号とみなされ「[REDACTED]」になる | closed | [#63](https://github.com/mizukidayo1025/-IF-/issues/63) |
| [PR #191](pulls/191.md) | モデルの署名をメールアドレスとみなさないようにする（#187） | merged | 記録として保存 |
| [PR #192](pulls/192.md) | 施設の停止を予定の時刻に効かせ、使用中の人は使い終えるようにする（#188） | merged | 記録として保存 |
| [PR #193](pulls/193.md) | Runの記録の電話番号の検出を、保存の検査と同じ定義に揃える（#190） | merged | 記録として保存 |
| [Issue #194](issues/194.md) | 図面の変更の介入に付けたguardが、対象を見つけられず実行時に失敗する | closed | [#64](https://github.com/mizukidayo1025/-IF-/issues/64) |
| [Issue #195](issues/195.md) | 環境の変更の介入で、guardで開始が抑制されると、期限切れのイベントが「失敗」として記録される | closed | [#65](https://github.com/mizukidayo1025/-IF-/issues/65) |
| [Issue #196](issues/196.md) | guardのeq・neが数値の型（整数か小数か）で比べ、0と0.0が一致しない | closed | [#66](https://github.com/mizukidayo1025/-IF-/issues/66) |
| [PR #197](pulls/197.md) | EffectProgramの検証とguardを生成Scenarioで確かめ、見つかった不具合をxfailで置く | merged | 記録として保存 |
| [PR #198](pulls/198.md) | 図面の変更の介入のguardを、その対象で評価する（#194） | merged | 記録として保存 |
| [PR #199](pulls/199.md) | guardで抑制した環境の変更の期限切れを抑制のままにし、guardの数値を値で比べる（#195・#196） | merged | 記録として保存 |
| [Issue #200](issues/200.md) | 図面の候補抽出で、外壁に接する部屋が部屋の候補にならない | closed | [#67](https://github.com/mizukidayo1025/-IF-/issues/67) |
| [Issue #201](issues/201.md) | 取り込んだ図面のScenarioで、給水と配食が一度も始まらない | closed | [#68](https://github.com/mizukidayo1025/-IF-/issues/68) |
| [Issue #202](issues/202.md) | 図面の取り込みで、型や参照の誤りの一部が理由なしで落ちる | closed | [#69](https://github.com/mizukidayo1025/-IF-/issues/69) |
| [PR #203](pulls/203.md) | 図面の取り込みを、アップロードからScenarioの実行まで確かめるテストを足す | merged | 記録として保存 |
| [Issue #204](issues/204.md) | 図面の取り込みで、検証が拒否した理由が画面に届かない | closed | [#70](https://github.com/mizukidayo1025/-IF-/issues/70) |
| [PR #205](pulls/205.md) | 取り込んだ図面で、給水台・配食台の担当職員をその設備のZoneに置く | merged | 記録として保存 |
| [PR #206](pulls/206.md) | 図面の取り込みで、誤った入力を理由付きで拒否し、その理由を画面に返す | merged | 記録として保存 |
| [PR #207](pulls/207.md) | 図面の候補抽出で、外壁に接する部屋も部屋の候補にする | merged | 記録として保存 |
| [Issue #208](issues/208.md) | 公開サービスの接続が切れると、Scenarioのプレビュー全体が失敗する | closed | [#71](https://github.com/mizukidayo1025/-IF-/issues/71) |
| [Issue #209](issues/209.md) | e-Statの表に「総数」の行があると、統計を使わずに仮定の年齢構成でScenarioを作り、そのことを知らせない | closed | [#72](https://github.com/mizukidayo1025/-IF-/issues/72) |
| [PR #210](pulls/210.md) | 外部データの接続を、偽の公開サービスでProject画面のプレビューから確かめるテストを足す | merged | 記録として保存 |
| [PR #211](pulls/211.md) | 公開サービスの接続が切れても、地域の調べを続けてプレビューを返す | merged | 記録として保存 |
| [PR #212](pulls/212.md) | e-Statの統計を合計の行を除いて使い、使えなかったときは知らせる | merged | 記録として保存 |
| [Issue #213](issues/213.md) | 履歴索引を再構築すると、実行中に記録した行動の判断の記録が消える | closed | [#73](https://github.com/mizukidayo1025/-IF-/issues/73) |
| [PR #214](pulls/214.md) | storageのうち利用者の操作から届く経路（CLI・計測・履歴索引・中断の回復）にテストを足す | merged | 記録として保存 |
| [PR #215](pulls/215.md) | 分岐と一致している履歴索引は作り直さず、判断の記録を守る | merged | 記録として保存 |
| [PR #216](pulls/216.md) | docs/testing.md を決まりと手順だけに整理し、毎回書き換える表と経緯を消す | merged | 記録として保存 |
| [PR #217](pulls/217.md) | 保存時の索引登録をまとめて行い、容量チェックの走査を速くする | merged | 記録として保存 |
| [PR #218](pulls/218.md) | 施設形状と空間制約の解析をtickのphase内で共有する | merged | 記録として保存 |
| [PR #219](pulls/219.md) | Checkpoint保存で、保存済みの履歴の変換結果を再利用する | merged | 記録として保存 |
| [Issue #220](issues/220.md) | 整合性検証の範囲と頻度を決める（毎tickと保存時） | open | [#74](https://github.com/mizukidayo1025/-IF-/issues/74) |
| [Issue #221](issues/221.md) | 履歴索引への登録が後の時刻ほど遅くなる（書込みキャッシュの大きさを決める） | closed | [#75](https://github.com/mizukidayo1025/-IF-/issues/75) |
| [PR #222](pulls/222.md) | 分岐の作成で、分岐元のCheckpointを1回だけ作って子branchと分岐の記録に使う | merged | 記録として保存 |
| [Issue #223](issues/223.md) | 分岐した子branchの履歴索引を、全履歴から作り直さずに作る | closed | [#76](https://github.com/mizukidayo1025/-IF-/issues/76) |
| [PR #224](pulls/224.md) | 保存とhistory indexの個人情報の検査と検証器の組み立てを、結果を変えずに速くする | merged | 記録として保存 |
| [PR #225](pulls/225.md) | PR #224 で誤って commit した下書きファイルを削除する | merged | 記録として保存 |
| [PR #226](pulls/226.md) | 保存済みCheckpointの読込で、文書のコピーと2回目の復元をなくす | merged | 記録として保存 |
| [PR #227](pulls/227.md) | イベントのpayloadの凍結・解凍で、JSONのスカラー値をすぐに返す | merged | 記録として保存 |
| [Issue #228](issues/228.md) | モデルの状態検証を、検証する内容を変えずに速くする（モデル担当者との調整が必要） | closed | [#77](https://github.com/mizukidayo1025/-IF-/issues/77) |
| [PR #229](pulls/229.md) | 乱数のキーを json.dumps を使わずに書き、引く値は変えない | merged | 記録として保存 |
| [PR #230](pulls/230.md) | 保存時の個人情報の検査で、一致しえない文字列に秘密情報の正規表現をかけない | merged | 記録として保存 |
| [PR #231](pulls/231.md) | 計算の再開と分岐で、最初の保存が読み込んだEventLogを再利用する | merged | 記録として保存 |
| [PR #232](pulls/232.md) | Checkpointの検証で、世界の正準JSONを1回だけ作ってハッシュと復元に使う | merged | 記録として保存 |
| [PR #233](pulls/233.md) | 「@」のない文字列の個人情報の検査で、メールアドレスの正規表現をかけない | merged | 記録として保存 |
| [Issue #234](issues/234.md) | 循環参照の回収（gc）に1日の計算の約7%がかかっている | closed | [#78](https://github.com/mizukidayo1025/-IF-/issues/78) |
| [PR #235](pulls/235.md) | 体調モデルの検証で、設定の照合を組み合わせなしに行い、関係のキーの解析を覚える（モデル担当者の確認待ち） | merged | 記録として保存 |
| [PR #236](pulls/236.md) | 関係の検証で、キーごとの判定結果を覚える（モデル担当者の確認待ち） | merged | 記録として保存 |
| [Issue #237](issues/237.md) | 容量の確認がストア全体を数え直すため、Runが増えるほど保存が遅くなる | closed | [#79](https://github.com/mizukidayo1025/-IF-/issues/79) |
| [PR #238](pulls/238.md) | 情報のモデルに渡すスナップショットを、使われない監査用ハッシュなしで作る | merged | 記録として保存 |
| [PR #239](pulls/239.md) | 世界のハッシュを、正準JSONを読み戻さずに計算する | merged | 記録として保存 |
| [PR #240](pulls/240.md) | 再生用のframeの一括読み取りで、分ごとのハッシュを増分で計算し、使わない比較を作らない | merged | 記録として保存 |
| [PR #241](pulls/241.md) | 再生の各branchの最初の分も、復元元のEventLogを再利用してハッシュを計算する | merged | 記録として保存 |
| [PR #242](pulls/242.md) | 計算中とCheckpointの読み込み中だけ循環参照の回収を減らす（#234 の判断待ち） | merged | 記録として保存 |
| [PR #243](pulls/243.md) | Checkpointの保存ごとにディスクを1回だけ数え、自分の書き込みを足して容量を確かめる（#237 の判断待ち） | merged | 記録として保存 |
| [PR #244](pulls/244.md) | history indexの書き込みキャッシュを64 MiBにする（#221 の判断待ち） | closed | 記録として保存 |
| [PR #245](pulls/245.md) | 同じCheckpointを読み直すとき、確認は行ったうえで復元した世界の複製を使う（#46 の判断待ち） | merged | 記録として保存 |
| [PR #246](pulls/246.md) | 分岐した子branchのindexに、分岐元のイベント由来の行を写す（#223 の判断待ち） | merged | 記録として保存 |
| [PR #247](pulls/247.md) | PDFレポートの2本の比較で、共通の起点をframeを作らずに読む | merged | 記録として保存 |
| [PR #248](pulls/248.md) | 人と世帯の履歴の検索で、近い時刻どうしは復元せずにtickを進めて作る | merged | 記録として保存 |
| [PR #249](pulls/249.md) | 結果への質問の比較で、すでに読んだ自分のbranchのframeを使い回す | merged | 記録として保存 |
| [PR #250](pulls/250.md) | 介入の適用で、分岐元の状態の確認を分岐の中で行い、復元を1回にする | merged | 記録として保存 |
| [PR #251](pulls/251.md) | 分岐どうしの最初の違いの検索（world）で、世界のハッシュを差分で計算する | merged | 記録として保存 |
| [PR #252](pulls/252.md) | 介入やイベントの適用で、世界の候補を作るときの余分なコピーを外す | merged | 記録として保存 |
| [PR #253](pulls/253.md) | Checkpointの保存で、同じ計算の中で前に保存したchunkのkeyを使い回す | merged | 記録として保存 |
| [PR #254](pulls/254.md) | frameのキャッシュを書くとき、frameの正準JSONを1回だけ作る | merged | 記録として保存 |
| [PR #255](pulls/255.md) | 保存ごとのハッシュ計算で、巨大な履歴のbyte列を一度につなげずに渡す | merged | 記録として保存 |
| [Issue #256](issues/256.md) | 履歴索引への登録で書き込む量を減らす（索引の中身と読み方を見直す） | open | [#80](https://github.com/mizukidayo1025/-IF-/issues/80) |
| [PR #257](pulls/257.md) | 計算を進めるときの Checkpoint の読み込みでは、復元した世界の複製を持たない | merged | 記録として保存 |
| [PR #258](pulls/258.md) | 使われなくなった復元済みの世界の複製を、60秒で捨てる | merged | 記録として保存 |
| [Issue #259](issues/259.md) | 計算中に、保存の直後の読み取りが「no checkpoint exists before requested minute」で失敗する | closed | [#81](https://github.com/mizukidayo1025/-IF-/issues/81) |
| [PR #260](pulls/260.md) | Webサーバーの計算を、計算ごとに別プロセスで行う（#48 段階1） | merged | 記録として保存 |
| [PR #261](pulls/261.md) | 保存の直後でも、計算済みの時刻の frame・比較・再生を読めるようにする | merged | 記録として保存 |
| [Issue #262](issues/262.md) | 計算中に、保存の直後の履歴の検索が history_index_stale で失敗する | closed | [#82](https://github.com/mizukidayo1025/-IF-/issues/82) |
| [PR #263](pulls/263.md) | Webサーバーの分岐を、別プロセスで作る（#48） | merged | 記録として保存 |
| [PR #264](pulls/264.md) | 保存の直後でも、履歴の検索・Checkpoint の一覧・会話記録を読めるようにする | merged | 記録として保存 |
| [PR #265](pulls/265.md) | 1つの介入で、一時外出と帰還を指示できるようにする | merged | 記録として保存 |
| [PR #266](pulls/266.md) | 介入の追加先を、Scenario のデータで絞って決める | merged | 記録として保存 |
| [Issue #267](issues/267.md) | 介入の対象：AI 経路での設備の停止の範囲、選んだ対象の渡し方、ライブ API での確認（#35 の残り） | open | [#83](https://github.com/mizukidayo1025/-IF-/issues/83) |
| [PR #268](pulls/268.md) | 履歴索引を WAL 方式で書き、書き込み中も読み取りを待たせない | merged | 記録として保存 |
| [Issue #269](issues/269.md) | 履歴索引を WAL にしてから、1日分の計算で索引への登録が時々10〜15秒長くなる | closed | [#84](https://github.com/mizukidayo1025/-IF-/issues/84) |
| [PR #270](pulls/270.md) | 履歴索引の WAL 方式をやめ、従来のジャーナル方式に戻す | merged | 記録として保存 |
| [PR #271](pulls/271.md) | Webサーバーで、異なる Run を同時に計算できるようにする（#48 段階2） | merged | 記録として保存 |
| [Issue #272](issues/272.md) | 100人・7日間の計算時間とメモリ・容量の記録（develop 4a3c553） | open | [#85](https://github.com/mizukidayo1025/-IF-/issues/85) |
| [PR #273](pulls/273.md) | テキストの読み書きと CLI の出力を、どの OS でも UTF-8 にする | merged | 記録として保存 |
| [PR #274](pulls/274.md) | Windows でのプロセスの確認・Run の削除・ファイルの共有などを直す | merged | 記録として保存 |
| [PR #275](pulls/275.md) | Anthropic の SDK を、最初に API を呼ぶときに読み込む | merged | 記録として保存 |
| [PR #276](pulls/276.md) | オフラインCIを3つの OS で流し、develop への PR ごとに自動で走らせる | merged | 記録として保存 |
| [PR #277](pulls/277.md) | 同じ Run の複数の branch を同時に計算・分岐できるようにする（#48 段階3 バックエンド） | merged | 記録として保存 |
| [PR #278](pulls/278.md) | Project 画面で、branch ごとの計算の状態を表示・操作する（#48 段階3 画面） | merged | 記録として保存 |
| [PR #279](pulls/279.md) | Checkpoint の chunk を保存ごとに1つの pack にまとめ、7日分の計算を19%短くする | merged | 記録として保存 |
| [PR #280](pulls/280.md) | 保存ごとの復元の確認を、直前に検証した世界との比較にして、7日分の計算を短くする | merged | 記録として保存 |
| [PR #281](pulls/281.md) | 運営の記録を足すときの検証を前の tick 以降の記録に絞り、日を追って伸びる tick の時間を減らす | merged | 記録として保存 |
| [PR #282](pulls/282.md) | 完全な検証で EventLog の確かめる種類と増えた記録だけを読み、保存を短くする | merged | 記録として保存 |
| [PR #283](pulls/283.md) | スレッドの計算が終わるとき、状態・書き込み権・ジョブをまとめて片付け、直後の計算が断られないようにする | merged | 記録として保存 |
| [PR #284](pulls/284.md) | 毎 tick の社会関係・体調の関係の確認を直接書き、検証の時間を減らす | merged | 記録として保存 |
| [PR #285](pulls/285.md) | 計測器で、保存済みの Run の終わりだけを測り、ハッシュを自動で比べられるようにする | merged | 記録として保存 |
| [PR #286](pulls/286.md) | 記録を出さない tick では、心理の更新で人ごとの記録を組み立てない | merged | 記録として保存 |
| [PR #287](pulls/287.md) | 保存済みの時刻で分岐するとき、読み込んだ Checkpoint を子にそのまま使う | merged | 記録として保存 |
| [Issue #288](issues/288.md) | 分岐が保存の途中で失敗すると、索引のない子 Branch が残る | closed | [#86](https://github.com/mizukidayo1025/-IF-/issues/86) |
| [PR #289](pulls/289.md) | 2つの Branch の同時計算のテストを、計算の速さに依存しない形にする | merged | 記録として保存 |
| [PR #290](pulls/290.md) | 分岐した子 Branch の履歴を、分岐元の行を写さずに系譜をたどって読む | merged | 記録として保存 |
| [PR #291](pulls/291.md) | 分岐した子 Branch の最初の保存で、分岐元の chunk を参照して書き直さない | merged | 記録として保存 |
| [PR #292](pulls/292.md) | Checkpoint の読み込みで、イベントの payload を JSON にし直さず、分岐では読んだ世界の写しを取っておかない | merged | 記録として保存 |
| [PR #293](pulls/293.md) | 長い計算では、索引への登録を別のプロセスで計算と並行して行う | merged | 記録として保存 |
| [PR #294](pulls/294.md) | 分岐が保存の途中で失敗したとき、子 Branch の pointer を残さない | merged | 記録として保存 |
| [PR #295](pulls/295.md) | 保存の復元の確認で、文書を写さず、前の確認の EventLog に増えた記録だけを足す | merged | 記録として保存 |
| [PR #296](pulls/296.md) | 審査終了に伴い develop を main へ統合する | merged | 記録として保存 |
| [PR #297](pulls/297.md) | feature/rust-port 宛ての PR でもオフライン CI を動かす | merged | 記録として保存 |
| [PR #298](pulls/298.md) | tick の計算を Rust に移し、段階内の繰り返しをなくす（段階 B2） | merged | 記録として保存 |
| [PR #299](pulls/299.md) | tick の終わりの検証を、変わりうるものだけにする（段階 B2） | merged | 記録として保存 |
| [PR #300](pulls/300.md) | 保存を速くし、Checkpoint のハッシュを chunk の木にする（段階 B3） | merged | 記録として保存 |
| [Issue #301](issues/301.md) | stress_sensitivity が0の人に接触が生じると、社会の伝播の更新が ValueError で止まる | open | [#87](https://github.com/mizukidayo1025/-IF-/issues/87) |
| [PR #302](pulls/302.md) | 削除した試作ブランチの名前を文書とコメントから除く | merged | 記録として保存 |
| [PR #303](pulls/303.md) | 2つの Run の同時計算のテストを、計算の速さに依存しないようにする | merged | 記録として保存 |
| [PR #304](pulls/304.md) | 保存の完全検証で、変わっていない履歴を読み直さない（段階 B3） | merged | 記録として保存 |
| [PR #305](pulls/305.md) | tick の段階の繰り返しをなくし、イベントの確認を Rust にする（段階 B4） | merged | 記録として保存 |
| [PR #306](pulls/306.md) | tick の細かい残りを減らす（手順 1 の B） | merged | 記録として保存 |
| [PR #307](pulls/307.md) | 保存の残りを減らし、索引のプロセスを軽くする（手順 1 の A） | merged | 記録として保存 |
| [PR #308](pulls/308.md) | Rust の基盤を作り、境界コストを計測する（P3） | merged | 記録として保存 |
| [Issue #309](issues/309.md) | 通路の閉鎖の期限切れと支援の時間切れが同じ分に来ると、Run が StateInvariantError で止まる | open | [#88](https://github.com/mizukidayo1025/-IF-/issues/88) |
| [Issue #310](issues/310.md) | 期限と guard を持つ職員の不在の介入で、Run が止まる | open | [#89](https://github.com/mizukidayo1025/-IF-/issues/89) |
| [PR #311](pulls/311.md) | 全経路の確認、決定性の確認、KPI の比較を追加する（手順 2） | merged | 記録として保存 |
| [PR #312](pulls/312.md) | tick の段階と WorldView、接触の kernel を機械的に分割する（P2） | merged | 記録として保存 |
| [PR #313](pulls/313.md) | 2つの Run の最初の差を見つけるツールを追加する（P0） | merged | 記録として保存 |
| [PR #314](pulls/314.md) | 追跡するミラーで書き込みの漏れを確かめ、3か所を直す（P1） | merged | 記録として保存 |
| [PR #315](pulls/315.md) | 手順3の設計を日本語の文書にし、7日分の時間を更新する | merged | 記録として保存 |
| [PR #316](pulls/316.md) | WorldCore と本番用の追跡ミラー、レーンの登録表を入れる（手順3 P4） | merged | 記録として保存 |
| [PR #317](pulls/317.md) | 人・施設・職員などの実体を Rust の行に持たせ、dataclass を写しにする（手順3 P6） | merged | 記録として保存 |
| [PR #318](pulls/318.md) | 心理の名前空間と metric_state を Rust に持たせ、雛形にする（手順3 P5） | merged | 記録として保存 |
| [PR #319](pulls/319.md) | 社会の伝播の状態と計算を Rust に移す（手順3 M1 レーン B） | merged | 記録として保存 |
| [PR #320](pulls/320.md) | 情報の伝播の状態と計算を Rust に移す（手順3 M1 レーン C） | merged | 記録として保存 |
| [PR #321](pulls/321.md) | 毎分の心理の計算と書き込みを Rust に移す（手順3 M1 レーン A） | merged | 記録として保存 |
| [PR #322](pulls/322.md) | 職員の動態を Rust へ移す（試験移植） | merged | 記録として保存 |
| [PR #323](pulls/323.md) | 保存の経路で変わっていない実体の符号化を省く | merged | 記録として保存 |
| [PR #324](pulls/324.md) | Policy が読む世界の view を Rust で作る | merged | 記録として保存 |
| [PR #325](pulls/325.md) | movement_v1 の状態を Rust の型付きの行で持つ | merged | 記録として保存 |
| [PR #326](pulls/326.md) | 毎 tick の検証の整理と、差し替えた世界の保存の修正 | merged | 記録として保存 |
| [PR #327](pulls/327.md) | 行動の提案を Rust へ移す | merged | 記録として保存 |
| [PR #328](pulls/328.md) | イベントの記録と予定の列を Rust で持つ | merged | 記録として保存 |
| [PR #329](pulls/329.md) | 行動の周期と agent_dynamics_v1 を Rust へ移す | merged | 記録として保存 |
| [PR #330](pulls/330.md) | social_network_v1 と社会的な行動の処理を Rust へ移す | merged | 記録として保存 |
| [PR #331](pulls/331.md) | body_state_v1 と毎 tick の書き手を Rust へ移す | merged | 記録として保存 |
| [PR #332](pulls/332.md) | 行動の判断の記録を Rust へ移す（M2 J） | merged | 記録として保存 |
| [PR #333](pulls/333.md) | 移動の規則を Rust へ移す（M4） | merged | 記録として保存 |
| [PR #334](pulls/334.md) | 物理的な列の毎 tick の処理を Rust へ移す（M4） | merged | 記録として保存 |
| [PR #335](pulls/335.md) | 資源の台帳を Rust の行で持つ（M4） | merged | 記録として保存 |
| [PR #336](pulls/336.md) | 職員の運用と共有運用の記録を Rust へ移す（M4） | merged | 記録として保存 |
| [PR #337](pulls/337.md) | 毎 tick の指標と実体の検証を Rust へ移す（M5） | merged | 記録として保存 |
| [PR #338](pulls/338.md) | サービスの処理と単純な判断の効果を Rust へ移す（M4） | merged | 記録として保存 |
| [PR #339](pulls/339.md) | イベントの dispatch を Rust へ移す（M3） | merged | 記録として保存 |
| [PR #340](pulls/340.md) | 人口と仮設の部屋の文書を Rust で持ち、毎分の比較を省く（M6） | merged | 記録として保存 |
| [PR #341](pulls/341.md) | EffectProgram の実行を Rust へ移す（M6） | merged | 記録として保存 |
| [PR #342](pulls/342.md) | 残りのイベントハンドラーとケアの受領を Rust へ移す（M6） | merged | 記録として保存 |
| [PR #343](pulls/343.md) | 人口の規則に Rust の経路を作る（M6） | merged | 記録として保存 |
| [PR #344](pulls/344.md) | tick の段階を Rust で回す driver を入れる（M6） | merged | 記録として保存 |
| [PR #345](pulls/345.md) | geometry_overlay と facility_queues の規則を Rust へ移す（M6） | merged | 記録として保存 |
| [PR #346](pulls/346.md) | 毎 tick の検証と指標の記録を Rust へ移す（M6） | merged | 記録として保存 |
| [PR #347](pulls/347.md) | 手順 4 の計画と kernel の拒否の実測を入れる（L0） | merged | 記録として保存 |
| [PR #348](pulls/348.md) | 読むだけの規則と zone の式を Rust へ移す（L5） | merged | 記録として保存 |
| [PR #349](pulls/349.md) | 残りのハンドラーをすべて Rust へ移す（M6） | merged | 記録として保存 |
| [PR #350](pulls/350.md) | trace の文書と Provider の経路を Rust と driver へ移す（L6） | merged | 記録として保存 |
| [PR #351](pulls/351.md) | 職員の割り当ての残りを Rust へ移す（L2） | merged | 記録として保存 |
| [PR #352](pulls/352.md) | 提案と移動の残りを Rust へ移す（L3） | merged | 記録として保存 |
| [PR #353](pulls/353.md) | 列・資源・人口の残りとすぐ閉じられる拒否を Rust へ移す（L1、L4） | merged | 記録として保存 |
| [PR #354](pulls/354.md) | 毎 tick の検証とその記憶を Rust へ移す（L7） | merged | 記録として保存 |
| [PR #355](pulls/355.md) | 保存されるイベントの文を Rust で出す（L8a） | merged | 記録として保存 |
| [PR #356](pulls/356.md) | モデル版の違う Run を閲覧専用にする仕組みを入れる（L9、版はまだ上げない） | merged | 記録として保存 |
| [PR #357](pulls/357.md) | イベント以外のエラー文を Rust で出す（L8b） | merged | 記録として保存 |
| [PR #358](pulls/358.md) | 保存されるイベントの文の残りを Rust で出す（L8c） | merged | 記録として保存 |
| [PR #359](pulls/359.md) | WorldState v5 の毎 tick の検証を Rust で行う（L7b） | merged | 記録として保存 |
| [Issue #360](issues/360.md) | Rust 移植の現状（Python のモデルと完全に同一）とこれからの作業、高速化の見込み | open | [#90](https://github.com/mizukidayo1025/-IF-/issues/90) |
