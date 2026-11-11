# 手順5の計画：PyO3 に依存しない中核の crate に分ける

Issue #152（#132 の段階3）の計画と記録です。Rust のコード（`native/`）を、PyO3 に依存しない中核の crate `sheltersim_core`（`native/core`）と、PyO3 の薄い窓口の crate `sheltersim_native`（`native/src`）に分けます。

## 決定（2026-11-11、利用者）

1. **実体：** 実体（`world.agents`、`facilities`、`staff`、`support_requests`、`active_tasks`）の持ち主を Rust にします。Python の dataclass は、行から作る読み取りの実体にするか、窓口を通して書く形にします。
2. **tick の中の Python：** シミュレーションの規則は Rust に移します。LLM の境界と、外への口（`capture_metric`、trace）は、中核の trait（`Host`）にします。ネイティブ版の `Host` は今の Python を呼びます。
3. **速さの予算：** 7日分（100人、project の経路、生成 Scenario）の計算時間は +5% までです。
4. **中核の範囲：** Scenario から世界を組み立てる処理と、Checkpoint の復元は、中核に入れません（段階4で決めます）。

ゲートと7日分のハッシュは変えません。`ENGINE_VERSION` も上げません。

## crate の形

- `native/Cargo.toml` は、2つの crate の workspace です。lint（`[workspace.lints]`）と `native/clippy.toml` は、両方に効きます。CI は `cargo clippy --workspace` で禁止メソッドを確かめます。
- `sheltersim_core` は `pyo3` に依存しません。CI の lint の job が、`cargo tree -p sheltersim_core` に `pyo3` がないことを確かめ、`cargo test -p sheltersim_core` を実行します。
- 中核へ移したモジュールは、`native/src/core/` の同じ名前のファイルが、元の道筋（`crate::core::mathx` など）で re-export します。そのため、lane のファイルの `use` は変えずに済みます。窓口のファイルには、Python が要る部分だけを残します：Python の例外への変換、Python の数や文字列との変換、`#[pyfunction]` の入口。
- **エラー：** 中核のエラー型から `PyErr` への `From` は書けません（どちらの型も窓口の crate の外にあるため）。窓口では、変換の関数（例：`mathx::math_error`）を `.map_err(...)` で使います。

## 棚卸し（2026-11-11、`main` `19692f8`）

6つに分けて、読んで数えました（何も実行していません）。PyO3 に触れる行の数え方は #152 の本文と同じです。

| 範囲 | ファイル | 行 | PyO3 に触れる行 | 大きいもの |
|---|--:|--:|--:|---|
| `src/core`、`src/state`、最上位のファイル | 37 | 14,477 | 2,101 | `world_core.rs`（XL）、`row.rs`（L）、`state/events.rs`（L）、`eventlog.rs`（L） |
| 運営の規則（職員、支援、トイレ、待ち行列、資源、行動） | 13 | 13,889 | 2,349 | `staff.rs`（XL）、`service.rs`・`support.rs`・`staff_operation_events.rs`（L） |
| 人の規則（心理、行動、判断、提案、社会、情報、身体） | 13 | 10,759 | 1,710 | `proposals.rs`（L） |
| 人口と移動（人口、移動、経路、近さ、区域の環境、再計画） | 9 | 12,006 | 1,709 | `population.rs`・`population_lifecycle.rs`（L） |
| tick の順序とイベント（tick、dispatch、handler、効果、overlay、検証、指標、trace、world view） | 11 | 14,705 | 3,057 | `handlers.rs`・`effects.rs`（XL） |

わかったこと：

1. **分けにくいのは、データではなく trait です。** 行（`state_row!` の行、`V`、実体の行、イベントの行、EventLog の索引）は、もう Python のハンドルを持ちません。しかし `Field`、`Row`、`Items`、`Write`、`Misfit` の型に PyO3 の型が出てきます。`write_json` が `PyResult` を返すのは、float が NaN のときに断るためだけです。
2. **世界の状態は、Python から見える約24個のオブジェクトに散っています**（`WorldCore`、19の `NamespaceMirror`、`EntityStore`、`QueueStore`、`EventStore` など）。kernel はそれぞれを getattr で探します。Rust の表現がまだないもの：スカラー（`scenario_id`、`seed`、`minute_index` など）、`zones`、`zone_edges`、`resources`、`id_counters`、`operational_events_v1`・`zone_environment_v1`・`infrastructure_v1` と LLM の runtime の名前空間、型の決まった設定。
3. **tick の中で、世界を丸ごと写して差し替える処理があります。** `effects.rs` の13の handler、`handlers.rs`、`population_lifecycle.rs` は、`from_dict(to_dict())` で世界を写し、写しを変え、Python の `WorldState.validate()` を通してから `__dict__` を差し替えます。検証の文は EventLog の `detail` に入るので、ハッシュに届きます。
4. **Rust の関数がないイベントの型はありません。** 43の型すべてに Rust の関数があります。Python の `dispatch_one` と `settle` は fallback で、7日分では0回、ゲートでは1,486回です（主に、待ち行列の中で変わったイベント）。
5. **tick の中に残る Python の規則は、通常の経路で約100行です：** `_sync_movement_after_events`、`_finish_stopped_journey`、`_refresh_space`（経路のグラフを作り、`travel.rs` が読む）、`record_support_request`。fallback だけのもの：`dispatch_one`・`settle`、`domain/care_receipts.py` の `make_*_record`、`care_source_ids_for_agent`、`record_observed_contacts`、別のクラスの manager の `arbitrate`、`upgrade_information_runtime_state`。
6. **LLM の境界は5つです：** `_prioritize_support_request` と `consider_support_request`（`proposals.rs` から）、`_consider_social_contact_event`（dispatch から）、`consider_complaint_response`（`staff.rs` から）。どれも `model_state` に書き、イベントを予定します。offline でも断った回数を指標に数えるので、呼ぶ順番はゲートのハッシュに届きます。
7. **外への口：** 指標（`capture_metric`、1 tick に1回）、trace の文書（trace が有効なときだけ）、判断の記録（`record_decision_document` など）、段階の observer（1段階に2回）。
8. **設定は毎 tick 読み直しています：** `selected_models`、`_parsed_config`、`canonical_*_config`、`InformationSpreadConfig.from_mapping`、Python の ContextVar の scope。pydantic の文を含む失敗もあります。
9. **実体の Python のオブジェクトを書く Python のコードは、tick の中にはありません。** Rust が行を書いてからオブジェクトを書きます。tick の外では約12か所（Branch の命令、EffectProgram の予定、検証の正規化）です。テストは、266ファイルのうち約69ファイルが実体を作るか書き換えます。
10. **順番のハッシュの危険：** 実体の辞書の順番（指標は `world.agents` の挿入順で足します）、イベントの通し番号の順、新しく作る名前空間の辞書のキーの順、`format(x, "g")`・`str.casefold`・`repr` の Python と同じ結果。

## 方針（2026-11-11、私が決めたもの。変えられます）

- **E1. 下から順に：** 型 → 状態の持ち主 → kernel → tick の順序。どの PR でもハッシュは変えません。
- **E2. `V` を中心に分けます。** 中核の `Field` は `V` との行き来（`from_v`、`to_v`）と JSON の書き出しを持ち、Python との行き来は窓口の trait にします。まだ Python の木に書く setter は、中核の trait（`TreeSink`）を通します。（PR 3 で、Python との行き来は `V` を通さず、窓口の `PyField` にしました。理由は下の「PR 3」にあります）
- **E3. 中核の `World`：** 1つの世界の状態を、1つの Rust の構造体にまとめます。窓口の `WorldCore` がそれを持ち、kernel は `&mut World` を受けます。`World` は部分（`Shard`）に分けます：実体（区域と区域の辺も加わります）、スカラー、持つ項目と名前空間の `Slots`、あとで予定のイベントの列と EventLog（PR 6-5 まではそれぞれ `QueueStore`・`EventStore` が持ちます）。窓口の shim は `WorldCore` を共有で借り、使う部分だけを可変で借ります。違う部分は同時に借りられ、同じ部分を二重に借りると panic ではなく中核のエラー（`RuntimeError`）です。実体、区域、名前空間の木などの Python のオブジェクトは、`World` を読み書きする窓（view）にします。実体の辞書は挿入順を保ちます（`IndexMap`、`shift_remove`）。トランザクションの後の順は、設計として正規の順です（E5、E13）。
- **E4. Host を呼ぶ関数は、世界を借りたままにしません。** 中核の関数は `WorldCell`（`with(|world| ...)` で短く借りる）を受け、Host を呼ぶ間は借りません。今の「Python を呼ぶ間は borrow を持たない」と同じ規則です。そのためネイティブ版の Host（LLM の境界）は、今までどおり Python の view を通して書けます。
- **E5. 写して差し替える処理は、その場のトランザクションにします。** 始めに snapshot（部分の copy-on-write）と EventLog の印を取り、世界を正規の順に並べ直して（`canonicalize`）、本体は世界そのものに書きます。commit は snapshot を捨て、失敗や取り消しは snapshot と印に戻します。トランザクションは入れ子にしません。本当に写すのは `deepcopy` と、捨てる下見（preview）だけで、`World::fork` を使います。
- **E6. 候補の世界の丸ごとの検証**（`WorldState.validate`、`validate_simulator_extensions`）は、#152 では Host の `validate_candidate` にします。ネイティブ版は、候補の Python の view に今の Python の検証をかけます（手順4の D3「`validate_*` は Python に残す」と同じ考えです）。検証の文を中核で作るのは段階4に回します。`after_tick.rs`・`tick_validation.rs` の「問題なし」と答える検査は、中核に移します。
- **E7. 設定：** 設定には2つの範囲があります。`RunConfig` はコントローラーの初期化のときに作ります（Run、続き、Branch ごとに1つ）。`Configs` は `model_configs` の stamp が変わるたびに、窓口が解釈し直します（トランザクションの始めの正規化で変わったときも含みます）。解釈は例外を出さず、出た誤りを読む場所が再現するので、設定の誤りの文とそれが出る時点は変わりません。（2026-11-11、P7 の設計で直しました）
- **E8. 断り：** `Unsupported` を返す約240か所は、中核の `Error::Unsupported` にします。窓口で今と同じ `Unsupported`、`KernelRefusal` になります。
- **E9. 記録の bytes は中核が持ちます。** Python の `bytes` は、要るときに作ってキャッシュし、保存のキャッシュが頼る「同じオブジェクト」を保ちます。そのレーンで計算のピークのメモリーを測り、増やしません（#138）。
- **E10. 窓口に残すもの：** tick への patch や shadow の検出、断りの数の統計、`rules/parity.rs`（テスト用）。
- **E11. 残る Python の規則：** 通常の経路の4つは Rust に移します。届かない fallback は消し、消したテストを PR に挙げます。`upgrade_information_runtime_state` は tick ではなく復元の側（中核の外）に移します。
- **E12. 実体の view は書き込みもできる、一貫したキャッシュにします。** 付いたオブジェクトの本体は行で、`__dict__` は Python に戻るたびに行と等しいキャッシュです（遅延の descriptor にはしません）。約69のテストのファイルが、そのまま動くようにするためです。`vars(agent.needs)`、dataclass の `==`、enum の `is` を保ちます。付いたオブジェクトの `__dict__` へ直接書いても kernel には届きません（そう書く6つのテストのファイルは P8d で行への書き込みにします）。taint の意味は今のままです。
- **E13. 正規の JSON を通して写している道は、正規の順と、新しいオブジェクトになる効果（設計の F1〜F3）を保ちます。** 今 `from_dict(to_dict())` を通る道は、辞書のキーと実体を正規の順に並べ、型を JSON の型にし、入れ物とイベントのオブジェクトを新しくします。それがハッシュに届くので、変えてもハッシュが同じだと PR で示すまでは、トランザクションでも同じ順と効果を作ります。
- **E14. そのまま伝える設定の誤り**（Python の誤りをクラスも文も変えずに出す読む場所）は、窓口がもう一度解釈して、その誤りを出します。pydantic の `ValidationError` は文から作り直せないためです。中核は、Python のないビルド（段階4）のために、クラスの名前と文を持ちます。

## レーン

| レーン | 中身 | 前提 | 並列 |
|---|---|---|---|
| P1 | workspace と、Python に触れない部分（済み） | — | — |
| P2 | 中核のエラーの型、`V`、`Misfit`、JSON の書き出し | P1 | — |
| P3 | `Field`・`Row`・`Items` を中核と窓口に分け、`state_row!`・`str_enum!` を中核へ。17の名前空間の行のファイルを、何回かに分けて中核へ | P2 | 行のファイルは並列 |
| P4 | 実体の行、`EventRow`・`QueueStore`・`EventWriter`、EventLog の索引と読み込み、Checkpoint の bytes の kernel、`contacts.rs`、`records.rs` の id を中核へ | P2（行は P3） | 部分ごとに並列 |
| P5 | 中核の `World`：スカラー、区域、`resources`、`id_counters`、`V` の行の名前空間をまとめる。kernel は1つの口で `World` を得る | P3、P4 | — |
| P6 | 実体の持ち主を Rust に（行が本体、Python は view、挿入と削除の順番、`__dict__` の差し替え）。fork と commit | P5 | — |
| P7 | 型の決まった設定、`Geo` と経路のグラフを中核のキャッシュに | P5 | P6 と並列 |
| P8a〜d | kernel を中核へ：a 人の規則、b 運営の規則、c 人口と移動、d イベントの適用・効果・overlay・world view・指標・trace・検証 | P6、P7 | 4つ並列 |
| P9 | Host の trait と tick の順序を中核へ。残る Python の規則 | P8 | — |
| P10 | 確かめ：`cargo test` で中核だけの tick、`wasm32-unknown-unknown` のビルド、7日分のハッシュと計算時間、文書。#152 を閉じる | P9 | — |

## 進め方

各 PR で、全経路のゲート（`benchmarks/gate_all_paths.py --expect fixtures/gate-all-paths.json`）が 0 problems であること、offline テストが通ることを確かめます。状態の持ち方を変える PR では、7日分のハッシュと計算時間も比べます。

### PR 1：workspace と、Python に触れない部分（2026-11-11）

- `native/core` を作り、次を移しました。
  - `mathx.rs`：`MathError` から Python の例外への変換と、`Num` と Python の数との変換は窓口に残す。
  - `rng.rs`：draw log の `#[pyfunction]` は窓口に残す。
  - `census.rs`：数える部分を移し、Python のリストを作る部分と `#[pyfunction]` は窓口に残す。
  - `parallel.rs`：テスト用の `#[pyfunction]` は窓口に残す。
  - `interner.rs`：`Interner` と `Sym` を移し、Python のオブジェクトを持つ `PyStrings` と `EnumTable` は窓口に残す。
  - `json.rs`：`lib.rs` の `write_json_string` と `python_float_repr`、`Encoder::float` の数字の部分を移す。
- 中核の `cargo test`（`native/core/tests/basics.rs`）は、CPython 3.12 で求めた値と比べます。
- 1つ目の commit で5つのファイルを中身を変えずに移し、2つ目の commit で中身を変えました。`git log --follow` で、移す前の履歴をたどれます。

### PR 2：エラーの型と V（レーン P2）

- **エラーの型：** `sheltersim_core::error` に `Error` と `Result<T>` を置きました。`Error` の各値は、Python の1つのクラスに当たります：`Unsupported`、`KernelRefusal`（`Refused`）、`EventSuppressed` と `EventApplicationError`（`Outcome`）、印のついた `ValueError`、`ValueError`、`KeyError`、`RuntimeError`、`TypeError`、`StateInvariantError`、`ResourceLedgerError`、壊れた状態（`Corrupt`）、`MathError`。文を組み立てる関数（`refused_text`、`corrupt_text`）も中核にあります。
  - `Error::unsupported` は `#[track_caller]` で、`Unsupported::new_err` と同じく、呼んだ行を census に数えます。`value_error`、`invariant`、`ledger`、`corrupt` も、今までの出口と同じく、呼んだ行を数えます。中核の行は `core/src/json.rs` のように数えるので、`benchmarks/rule_shadow/refusal_census.py` の静的な一覧も `native/core/src` を読むようにしました。
  - 窓口の `core/errors.rs` では、`to_py_err` が `Error` を同じクラスと同じ文の `PyErr` にします。窓口に残るのは、クラスを探す部分だけです。中核の呼び出しは `core_call().or_py()?`（`OrPy`）と書きます。今までの出口（`value_error`、`corrupt` など）は、中核の `Error` を作って `to_py_err` を通します。Python から見える例外は変わりません。
- **V：** `V` と `is_truthy`、`write_json` を `sheltersim_core::value` に移しました。`write_json` は中核の `Encoder` に書きます。NaN と無限大は、今と同じ文の `Unsupported` です。Python の値との変換（`from_py`、`to_py`）は、窓口の `core/value.rs` の `VPy` に残しました。`Field` や `Row` と名前が重なるところは、`<V as VPy>::from_py` のように書きます。
- **Encoder：** 値を書く部分（null、bool、int、str、float）を中核の `json::Encoder` にしました。Python の値をたどる部分（`value`）と、`PyResult` を返す `float` は、窓口の trait `PyEncoder` です。`crate::Encoder` は中核の struct の re-export なので、`Encoder { out: ... }` はそのまま使えます。`encoder.float(...)` などを使うファイルには、`PyEncoder` の `use` を足しました。
- **Misfit：** 中核の `error.rs` に移しました。後で `row.rs` を `git mv` で中核へ移すときに、同じ名前のファイルがないようにするためです。`PyErr` からの `From` は書けないので、窓口の `OrMisfit`（`python_call().or_misfit()?`）にしました。理由の文は、今と同じ `PyErr` の表示です。
- `PyResult` を返す関数の中で中核の `V::write_json` を呼ぶ7か所に `.or_py()` を、`Misfit` を返す関数の中で Python を呼ぶ42か所に `.or_misfit()` を足しました。lane のファイルのほかの変更は、`use` だけです。
- 中核のテスト（`native/core/tests/error_value.rs`）は、エラーの文と、`V` の JSON のバイト列を確かめます。バイト列は、CPython 3.12 の `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)` で求めました。
- 1つ目の commit で `value.rs` を中身を変えずに移し、2つ目の commit で中身を変えました。

### PR 4a：EventLog、Checkpoint の bytes、イベントの行（レーン P4a）

- **中核の `events`：** `EventKind`、`EventStatus`、`is_stable_id`、`EventRow`（`new`、`write_json`、`record_json`、`from_saved_record`）、列の `Queue`（`queue.rs`）、`EventLog`（`log.rs`）、`writer` を、中核の1つの module にまとめました。窓口の `state::events` ↔ `eventlog` の循環はなくなりました。`checkpoint::encode_*` は Python の値をたどるので窓口に残り、中核からは呼びません。`is_stable_id` は3か所にあった同じ検査を1つにしました。
- **列：** `Queue` は slot、予定した順、`(due_minute, priority, sequence)` の順、ID と sequence の索引、`validate` を持ちます。窓口の `QueueStore` は `Queue` と、同じ serial の Python のオブジェクトの表を持ちます。`pending()` の tuple は、列を変えるたびに捨てていましたが、列の世代（`Queue::generation`）が作ったときと同じ間だけ使う形にしました。中核が列を変えても、古い tuple は使われません。
- **記録：** `EventLog` は、記録ごとの索引の欄、ここで書いた bytes、索引、監査を持ちます。窓口の `EventStore` は、位置ごとの Python のオブジェクト（記録と bytes）の表を持ちます。この表は記録の数より短くてよく、オブジェクトのない位置（Rust の規則が足した記録）は表にありません。中核の `writer::record` が、窓口を通らずに記録を足せるようにするためです。Python の `EventLog` の読み方（`of_types` の型の覚え方、`release_records`、`unread` など）は変えていません。
- **記録の bytes の持ち主は変えていません（E9 は後のレーン）。** Python が bytes を求めると、ここで書いた bytes を中核から取り出し（`EventLog::take_written`）、1つの Python の `bytes` にします。保存のキャッシュが頼る「同じオブジェクト」は今までどおりです。
- **writer：** 予定（`writer::schedule`）と記録（`writer::record`）の検査と、その順番と文を中核に置きました。`DuplicateEventError` は中核のクラスではないので、`WriteError::Duplicate` にして、窓口で同じクラスと文にします。予約した sequence（`Reserved`）と `MAX_RULE_COUNTER` も中核です。`id_counters` はまだ Python の dict なので、数の読み書きと `EventWriter::of` は窓口に残ります（P5）。
- **Checkpoint の bytes：** プライバシーの検査の候補、内容で決める chunk の区切り、chunk のハッシュ、`dumps_length` の数え方、`split_json_array`、SHA-256 の16進を、中核の `checkpoint.rs` に置きました。Python の `bytes` を読む部分と GIL を外す部分は窓口です。`index_row`、`same_saved_records`、`record_json` は Python の値をたどるので窓口に残ります。`records.rs` は `is_stable_id` と `sha256_hex` を中核から使い、ほかは窓口のままです。
- `contacts.rs` は、Python の数を読む `exact_float`・`exact_int` と、テスト用の `fsum` のほかを中核へ移しました。`pair_key` の断りは中核の `Error::unsupported` で、窓口は同じ名前で `PyResult` を返します。
- 中核へ移った `Unsupported` の場所（`pair_key`、`writer::schedule` の数の上限）は、census では中核の行に数えます。文とクラスは同じです。
- `EventRow` の `from_event` と `to_py` は窓口の trait `EventRowPy` です。それを使う8つの規則のファイルに `use` を足しました。規則のファイルのほかの変更はありません。
- 中核のテスト（`native/core/tests/events_bytes.rs`）は、chunk のハッシュ、chunk の区切り、`split_json_array` を、`main`（`0085254`）のビルドの Python から見える関数で求めた値と比べます。列の順番、`validate`、writer の文、書いた記録の bytes と読み戻しも確かめます。
- 確かめたこと：中核のテスト（16件）、`cargo clippy --workspace`、`cargo tree`（中核に `pyo3` なし）、offline テスト（5115 passed、6 skipped）、ゲート（55 scenarios、0 problems）。7日分（100人、project の経路、生成 Scenario）は、P2 のビルドと交互に2回ずつ測りました。world・events・KPI のハッシュは同じです。計算時間（wall）は P2 が 70.2 s・70.7 s、このPRが 70.7 s・71.5 s、thread の時間は 66.4 s・65.7 s と 66.3 s・65.1 s で、差は揺れの中です。計算のピークの RSS（`computation_mib`）は P2 が 895.1・888.3 MiB、このPRが 890.1・894.2 MiB で、増えていません。
- 1つ目の commit で `eventlog.rs` と `contacts.rs` を中身を変えずに移し（`core/src/events/log.rs`、`core/src/contacts.rs`）、2つ目の commit で中身を変えました。窓口の同じ名前の2つのファイルは新しいファイルです。`state/events.rs`、`checkpoint.rs`、`records.rs` は窓口に残る部分が多いので移さず、中核のファイルを新しく作りました。

### PR 3：行の trait と名前空間の行（レーン P3）

- **trait の分け方：** 中核の `sheltersim_core::row` に、`Field`（`write_json`、`is_absent`、`when_missing`、`items`、`part_items`）、`Items`（`count`、`item_json`）、`Row`、`Scalar`（`same`、`atom`）、`Part`、`ValueRow`、`IdMap`、`Maybe`、`map_set`、`sorted_order` を置きました。Python との行き来は、窓口の `PyField`（`from_py`、`to_py`、`apply`、`part_to_py`、`py_items`、`to_py_emptied`）、`PyItems`（`slice_to_py`）、`PyRow` です。`NamespaceMirror` は `Box<dyn PyRow>` を持ちます。`Write` は Python の値を持つので、窓口に残しました。
- **E2 と違うところ：** `V` を通す形（`from_v`、`to_v`）にはしませんでした。Python の値をいったん `V` にしてから行にすると、読み込みと転送された書き込みのたびに割り当てが増えます。Misfit の理由の文も変わります（例：`not a list` が `value is not a plain JSON value` になる）。そこで、Python との行き来は今のコードのまま窓口の `PyField` に残し、中核の構造体には `py_row!` で付けました。中核で行を作る口は、中核が状態を持つレーン（P5）で決めます。
- **マクロ：** 中核の `state_row!`・`str_enum!`（`#[macro_export]`、`$crate` の道筋）は、構造体、`NAMES`、`write_json`、setter を作ります。setter は中核の trait `TreeSink` に書き、窓口の `Tree` がそれを実装します。値は `Atom`（null、bool、int、float、str、集合の名前）で渡します。木に入る Python の値は前と同じです（集合の名前は interned の str、項目の名前も interned）。窓口の `py_row!` は、同じ名前を同じ順に並べて Python の側を作ります。順や数が違うと、コンパイルで止まります（`same_names`）。窓口の `state_row!`・`str_enum!` は、中核のマクロと `py_row!`・`py_str_enum!` を一度に書くもので、窓口だけの行に使います。
- **中核へ移した名前空間の行（12）：** `agent_dynamics`、`body_state`、`effect_program`、`facility_queues`、`information`、`metric_state`、`movement`、`psychological_distress`、`resource_service`、`social_network`、`social_propagation`、`staff_dynamics`。行と、行だけを読む検査（`is_valid` など）を `native/core/src/state/` に移しました。窓口の同じ名前のファイルは、行を `pub(crate) use` で元の道筋に見せ、`py_row!`、手で書いた `PyField`（`Journey`、`History`、`Relationships`、`Recovery`）、`register`、検証の入口を持ちます。Python から見える名前は変わりません。実体の行を読む検査は、`EntityStore` が窓口にあるうちは窓口に置くか（`resource_service` の `passes_rules`）、中核の関数に閉包で渡します（`agent_dynamics` と `information` の `is_valid`）。
- **残したもの：** `resource_ledger`（検証が `resources`、`agents`、`zones` の Python の dict を読む）、`shared_operations`（検証が `EventWriter` と `staff_operations` を読む）、`staff_operations`（実体のオブジェクトと予定のイベントを読む）は、窓口のまま `Field` と `PyField` に分けました。実体の行（P4）の後に移します。`sample_namespace` はテスト用なので窓口に残し、窓口の `state_row!` の例にしました。`population` には型の決まった行がなく（`ValueRow` は中核）、移すものはありません。
- 1つ目の commit で `row.rs` と12のファイルを中身を変えずに移し、2つ目の commit で中身を変えました。
- **中核のテスト**（`native/core/tests/rows.rs`）：中核の外から `state_row!`・`str_enum!` を使い、JSON のバイト列とキーの順、setter が同じビットの値を書かないこと、`psychological_distress` の行のバイト列と検査を確かめます。期待値は CPython 3.12 の `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False)` で求めました。
- **確かめたこと：** `cargo test -p sheltersim_core`、`cargo clippy --workspace -- -A clippy::all -D clippy::disallowed_methods`、`cargo tree -p sheltersim_core -e normal,build` に `pyo3` がないこと、offline の pytest（`-m "not long_simulation"`：5115 passed、6 skipped）、全経路のゲート（55 scenarios、0 problems）。PR 4a の後の `main`（`4834ebe`）の上で確かめました。
- **7日分**（100人、project の経路、生成 Scenario）：world、events、metrics のハッシュは `main`（`4834ebe`）と同じです。計算時間は、wall が `main` 64.9 s、この PR 65.2 s、thread が 61.2 s と 61.3 s で、差は揺れの中です。計算のピークの RSS は 898.1 MiB と 894.4 MiB で、増えていません。PR 4a の前に PR 2 の commit と交互に3回ずつ測ったときも、差は揺れの中でした（thread の平均 +2.7%、wall の平均 −1.3%、ほかのレーンの計算と同時）。

### PR 4b：実体の行（レーン P4b）

- **中核の `entities`**（`native/core/src/entities.rs`）：実体の行（`NeedRow`、`ProfileRow`、`AgentRow`、`FacilityRow`、`StaffRow`、`RequestRow`、`TaskRow`）、`Skills`、enum の `Activity`・`Mobility`・`RequestStatus`、`Table<R>`、`Entities`、表の番号（`AGENT`〜`TASK`）を置きました。持ち主は変えていません。Python のオブジェクトが本体で、行はその写しのままです（持ち主を変えるのは 6-1）。
- **trait の分け方：** 実体の行の欄は、名前空間の `row::Field` とは別の中核の `EntityField`（`same`、版を受ける `write_json`）です。v5 の文書は v6 の欄の一部を書かないので、版が要ります（`[v6]`・`[set]` の規則も中核の `entity_row!` にあります）。行の id を返す `EntityRow` が、文書の行の順（id の順、同じ id は dict の順）を決めます（`Table::document_slots`）。Python との行き来（`from_py`、`to_py`、転送された書き込み）は窓口の `PyEntityField` と `Forward` で、窓口の `py_entity_row!` が同じ欄を同じ順に並べます（順や数が違うとコンパイルで止まります）。
- **enum：** 中核は `str_enum!` で、メンバーの順と `.value` の文字を持ちます。文書の文字は中核のものを書きます。窓口の `install` は、Python の enum のメンバーの数、順、`.value` が中核と同じことを確かめ、違えば今と同じ文の `ValueError` です。Python のメンバーのオブジェクト（`agent.activity is Activity.IDLE`）は窓口が持ち、規則は窓口の trait `Member` の `member(py)` で得ます（`.member(py)` を使う11の規則のファイルに `use` を足しました）。`Activity::text` は失敗しなくなったので、2か所の `?` を消しました。
- **`Table`：** 欄を private にし、slot と key の操作を関数にしました（`reserve_slot`、`fill_slot`、`slot_mut`、`release_slot`、`bind`、`remove`（`shift_remove` と slot の解放）、`set_keys`、`reorder_keys`、`changed`・`mark_written`）。種類で表を選ぶ窓口の `match` は、`Entities::table(kind)`・`table_mut(kind)` が返す `dyn AnyTable` にしました。`NEEDS` と知らない番号が `active_tasks` になるのは今までの `match` と同じです。後のレーン（`keys_by_slot`、`dirty`、`journal`、`canonicalize`）は、これらの関数の中で足せます。`rows_json` の bytes のオブジェクトの表は窓口に残ります。
- **3つの名前空間の行：** `resource_ledger`、`shared_operations`、`staff_operations` の行と、行と実体の行だけを読む検査を `native/core/src/state/` へ移しました。窓口の同じ名前のファイルは、行を re-export し、`py_row!`・`py_str_enum!`、手で書いた `PyField`（`Audit`、`UsedIds`、`Heads`、`Receipts`）、検証の入口を持ちます。
  - `resource_ledger`：`passes_rules` の本体は中核の `passes_rules_in` で、世界の `resources`、`agents`、`zones` は trait `LedgerWorld` を通して読みます（窓口は Python の dict を読む `WorldDicts`）。読む順は今までと同じです。規則が呼ぶ `row.passes_rules(&resources, &agents, &zones)` は、窓口の trait `LedgerChecks` で今の形のままです（`rules/resources.rs` に `use` と、`audit.push` の `.or_py()` を足しました）。
  - `shared_operations`：`validate_tick` は EventLog の `find` の Python のオブジェクトを読むので、窓口に残しました。
  - `staff_operations`：行だけを読む検査（`active_records`）と実体の行を読む検査（`agrees_with`）を中核にしました。`validate` は今と同じ順で、先に行の検査、それが通ってから実体の行を借ります（汚れた store が `Unsupported` を言う時点は変わりません）。オブジェクトと予定のイベントを読む `active_records_pass` と `completion_agrees` は窓口です。
- 1つ目の commit で3つの名前空間のファイルを中身を変えずに移し（`git log --follow` でたどれます）、2つ目の commit で中身を変えました。`state/entities.rs` は窓口に残る部分（`EntityStore`）が多いので移さず、中核の `entities.rs` を新しく作りました。
- **中核のテスト**（`native/core/tests/entities.rs`）：5種類の行の v5 と v6 の JSON のバイト列（期待値は `main`（`174e229`）のビルドの `EntityStore.rows_json` が同じ Python のオブジェクトに書いたもの）、NaN の断りの文、enum の順と文字、`same` がビットで比べること、`Table` の key の順（`shift_remove`、空いた slot の再利用、同じ key は場所を保つ、`reorder_keys`、`set_keys`）と文書の行の順、`changed` の印。
- **確かめたこと：** `cargo test -p sheltersim_core`（31件）、`cargo clippy --workspace -- -A clippy::all -D clippy::disallowed_methods`、`cargo tree -p sheltersim_core -e normal,build` に `pyo3` がないこと、offline の pytest（`-m "not long_simulation"`：5115 passed、6 skipped）、全経路のゲート（55 scenarios、0 problems）。
- **7日分**（100人、project の経路、生成 Scenario）：`main`（`174e229`）のビルドと交互に2回ずつ測りました。world、events、metrics のハッシュは同じです。wall は `main` 64.7 s・64.5 s、この PR 63.9 s・64.4 s、thread は 60.8 s・60.4 s と 60.5 s・60.7 s で、差は揺れの中です。計算のピークの RSS（`computation_mib`）は 893.8・899.1 MiB と 897.2・893.8 MiB で、増えていません。

### PR 5-1：中核の World（レーン 5-1）

- **中核の型：** `native/core/src/cell.rs` に `Shard<T>`、`stamp.rs` に `Stamp`、`world.rs` に `World`、`Slots`、`Slot`、`Place`、`SlotCounts` を置きました。`World` は今 `entities: Shard<Entities>` と `slots: Slots` だけを持ちます。スカラー、区域、`resources`、`id_counters` は 5-2、ほかの名前空間の `ValueRow` は 5-3、予定のイベントの列と EventLog は 6-5、状態でない `derived` は 7-1 で足します（`world.rs` の冒頭に書きました）。
- **`Shard`：** PyO3 0.23 はすべての `#[pyclass]` に `Sync` を求めるので（`impl_/pyclass/assertions.rs` の `assert_pyclass_sync`）、旗は `AtomicIsize` です（`atomic_refcell` と同じ形）。`try_borrow`・`try_borrow_mut` は、同じ部分を二重に借りると panic ではなく `Error::Runtime`（窓口で `RuntimeError`）を返します。今までの PyO3 のセルの二重の借りも `RuntimeError` だったので、例外のクラスは同じです。設計の §2 は `Error::Corrupt` と書いていましたが、§4 の R3（`RuntimeError`）に合わせました。世界全体を持つ kernel は `get_mut` で旗を見ずに読みます。
- **`Slot`：** 持つ項目・名前空間ごとに、行（`Option<Box<dyn Row>>`）、taint、切り離しの印、世代、数（`forwards`、`whole_writes`、`rebuilds`）、`place`（`world.<name>` か `model_state` のキーか）を持ちます。世代は書くたびに `Stamp::next()`（プロセスで1つの atomic のカウンター）にしました（設計の §13：取り消しの後に古いキャッシュが当たらないように）。`generation_of` が返す数は前と同じく「書くたびに変わる数」で、値そのものはもう名前空間ごとの通し番号ではありません。`Slots` は登録の順で、slot を消さないので、窓は番号で自分の slot を引きます（名前の検索がありません）。実体の taint は `Entities` が持ちます（`taint` は行を捨て、最初の理由を残します）。
- **窓口：** `WorldCore` が `World` と、窓（slot ごとの `NamespaceMirror`、`EntityStore`）を持ちます。`NamespaceMirror` は core、slot の番号、名前、`RowType`、木の側（`Shard<TreeSide>`：木、節点の表、`items_json` で渡した bytes）だけを持ちます。`EntityStore` は core、`rows_json` の bytes（`Shard`）、数（atomic）だけです。どちらも `frozen` にしました。可変な部分が `Shard` の中にあるので、窓を借りる操作がなく、shim の atomic の操作は `WorldCore` の共有の借り1つと部分の借り1つです（前は窓の借り1つ）。窓は core を持ち、core は窓を持つので、`__traverse__` を両方に、循環を切る `__clear__` を `WorldCore` に書きました。
- **shim の形は同じです：** `with_row`、`with_row_tree`、`with_value`、`mirror_of`、`field_mirror_of`、`held_state`、`detached_part`、`apply_to_row`、`read_only_state`、`EntityStore::with_rows`、`entities_of` などは、名前と引数を変えていません。規則のファイルで変えたのは `common_operations.rs` の1か所だけです（窓が行への参照を返せなくなったので、`row_ref` の代わりに閉包の `with_py_row` を使います）。`mirror.try_borrow()?.holds_state(...)` のような書き方は `frozen` でもそのまま通ります。
- **`RowType`：** 行の型ごとの操作（`PyRowOps`：読み込み、`dyn Row` から `PyRow` を得る2つの関数）を持ちます。`Row` に `as_any` を足し、`OpsOf<T>` の関連定数で型ごとに1つの表を作ります。登録は `RowType::of::<行の型>("名前")` です（17のファイルの1行ずつ）。表は `py_row!` が書くのではなく、`PyField`（`py_row!` が書きます）と `ValueRow` に共通の `LoadRow` から作ります。
- **Python：** `_attach_fields` は `core.own(name, row, field=True)` を渡します（`Place::Field`）。ほかの Python は変えていません。
- **中核のテスト**（`native/core/tests/world.rs`、9件）：`Shard` の二重の借りがエラーになり panic しないこと、共有の借りが重なり書き手を入れないこと、`map` した借りが続くこと、違う部分は同時に借りられること、`Slots` の番号と二重の登録の断り（`ValueError` の文は前と同じ）、行の型での読み出しと断り、`Stamp` が slot とスレッドをまたいで重ならないこと。
- **確かめたこと：** `cargo test -p sheltersim_core`（40件）、`cargo clippy --workspace -- -A clippy::all -D clippy::disallowed_methods`、`cargo tree -p sheltersim_core -e normal,build` に `pyo3` がないこと、`ruff check .`、`ruff format --check .`、offline の pytest（`-m "not long_simulation"`：5114 passed、6 skipped、1 failed）、全経路のゲート（55 scenarios、0 problems）。落ちた1件（`test_refusal_census.py` の generated Run）は、子のプロセスの `PYTHONPATH` を `native/pylib` に置き換えるテストで、このレーンのビルドではなく共有の `.venv` に入った `main` のビルドを読んだためです（`own()` に `field` がない）。`native/pylib` にこのビルドを置くと、そのファイルの6件は通ります。CI はこのビルドを入れるので、そこでは起きません。
- **7日分**（100人、project の経路、生成 Scenario）：PR 4b の後（`75d6465`、`main` の `968779b` と同じ木）のビルドと交互に2回ずつ測りました。world、events、metrics のハッシュは同じです。wall は base 64.9 s・64.6 s、この PR 64.8 s・64.6 s、thread は 60.9 s・60.5 s と 61.0 s・60.9 s（平均 +0.4%）、`TickEngine.step` の thread は 16.8 s・16.6 s と 16.8 s・16.9 s（平均 +0.7%）で、揺れの中です。計算のピークの RSS（`computation_mib`）は 893.4・891.2 MiB と 888.8・894.0 MiB で、増えていません。

### PR 5-3：型の行のないキーの `V` の行（レーン 5-3）

- **何をしたか：** `model_state` のキーのうち、型の行を持たないもの（`operational_events_v1`、`infrastructure_v1`、`zone_environment_v1`、LLM の `social_runtime_v1`・`support_priority_runtime_v1`・`llm`、`facility_queues_legacy_mode_v1`、Scenario が持つ知らないキー）は、名前が `str` なら、初めて入れたときに `ValueRow` の `Slot` を持ちます。`NamespaceTable` が `_core` を持ち、`_value_mirror` が `WorldCore.own_value` を呼びます（`Place::Value`）。木は切り離さず、今までどおり持ちます（`DETACHED_NAMESPACES` には入れません）。
- **キーが出入りするとき：** キーを消す（`del`、`pop`、`popitem`、`clear`）と、木と行を捨てます（`clear_root`）。`Slot` とミラーは表に残り、同じキーをまた入れると同じミラーを使います（`Slots` は slot を消さないので、番号は変わりません）。`model_state` を丸ごと代入すると、ミラーと `_core` は新しい表に移り、古い表はもう何も持ちません。`deepcopy` は新しい core で持ち直します。
- **持たないもの：** 名前が `str` でないキー、`str` の派生型のキー、持つ項目の名前（`metric_state`、`model_configs`。core にその名前の slot があるため）、core のない表のキーは、今までどおり値をそのまま入れます。
- **型と順：** `V` は `1`、`1.0`、`True` を分け、辞書の順を保ちます。`model_state` の順は文書の順のままです。`V` に入らない値（tuple、集合、オブジェクトなど）は、そのキーの行だけを taint し、木は入れた値のままです。ほかのキーの行は変わりません。
- **変わること：** 入れた素の dict・list は、ほかの持つ名前空間と同じく `TrackedDict`・`TrackedList` の写しになります。入れる前の名前を通した書き込みは木に届きません。`src/` にはその書き方はなく、2つのテストを直しました（`test_world_core.py` の「ほかは素のまま」、`test_first_difference.py` の同一性の確認は、入れた後の木を名前にします）。`src/` の Python に `type(x) is dict` の読み方はありません。Rust で dict・list の型を完全一致で見る所（`records.rs` の記録の凍結、`checkpoint.rs`、`service.rs` の待ちの list）に木が渡ると、そこは今までの持つ名前空間と同じく Python の道になります。全経路のゲートのハッシュは同じで、`test_refusal_census.py`（生成 Run で fallback が0）も通ります。
- **中核：** `Slots::value`（世界全体を持つ kernel 用）と `Slots::try_value`（その slot だけを借りる shim 用）は、キーの `V` を返します。キーがなければ `None`、taint したキーと別の型の行は `Unsupported` です。`V::heap_bytes` は、測るための、`V` がヒープに持つバイト数です（窓口の `NamespaceMirror.value_bytes`）。
- **行と木が同じこと：** tick の境目ごとに、すべての `Place::Value` のミラーで `verify`（行と木の正規の JSON が同じ）を確かめる仕掛けを、測るときだけ入れて動かしました。全経路のゲート（55 scenarios、31,922回）と offline の pytest（80,389回）で、違い、taint、表と木のずれは0件でした。Rust の規則は、これらの名前空間に Python の呼び出し（`set_item`、`append` など）で書くので、木の override を通り、行に届きます。
- **中核のテスト**（`native/core/tests/world.rs`）：値の slot がない・行がない・値がある・taint・別の型の行のときの `value` と `try_value`、借りている間はその slot を書けないこと、`heap_bytes` の数え方。Python のテスト（`tests/unit/core/test_state_mirror.py`）：初めて入れたときの持ち方と型、出入りでミラーが残ること、順（持つ名前空間、値のキー、`str` でないキーが混ざるとき、丸ごと代入）、taint がそのキーだけのこと、持たないキー、`deepcopy`。`tests/integration/test_world_core.py`：断水のある生成 Scenario の Run の終わりに、すべての値の行が木と同じこと。
- **大きさ**（7日分、100人、project の経路、生成 Scenario。tick ごとに測りました）：行を持ったのは `operational_events_v1` と `zone_environment_v1` の2つで、`V` のヒープは最大 9,562 bytes と 263 bytes（正規の JSON で 1,716 bytes と 35 bytes）です。大きいものはありません。計算のピークの RSS は下のとおり増えていません。切り離すかどうかは、大きいキーが出てから決めます。
- **確かめたこと：** `cargo test -p sheltersim_core`（42件）、`cargo clippy --workspace -- -A clippy::all -D clippy::disallowed_methods`、`cargo tree -p sheltersim_core -e normal,build` に `pyo3` がないこと、`ruff check .`、`ruff format --check .`、offline の pytest（`-m "not long_simulation"`：5122 passed、6 skipped）、全経路のゲート（55 scenarios、0 problems）。
- **7日分**：PR 5-1 の後（`87dbfb0`、`main` の `21b50df` と同じ木）のビルドと交互に2回ずつ測りました。world、events、metrics のハッシュは同じです。ほかのレーンの計算と同時で、負荷が大きく揺れました（load average 7〜15）。wall は base 100.8 s・140.6 s、この PR 75.6 s・90.3 s、thread は 95.0 s・108.6 s と 71.1 s・84.3 s、`TickEngine.step` の thread は 24.2 s・27.7 s と 18.7 s・21.9 s で、どれもこの PR の回が速かったのは負荷の違いです。差を言える測り方ではありませんが、遅くなった印はありません。計算のピークの RSS（`computation_mib`）は 898.0・903.5 MiB と 893.6・898.0 MiB で、増えていません。

### PR 5-2：スカラー、区域、`resources`、`id_counters`（レーン 5-2）

- **スカラー：** 中核の `World` に `scalars: Shard<Scalars>` を足しました（`native/core/src/world.rs`）。`scenario_id`、`seed`、`duration_minutes`、`minute_index`、`schema_version` の5つで、どれも `Fit<T>` です。Python の値の型が違うと `Fit::Misfit` になり、型の決まった読み出し（`World::minute_index` など）は、今の `exact_int` と同じく `Unsupported` を返します。`seed` は `Seed { text, small }` です。`text` は `str(seed)`（大きさに上限のない Python の int の10進の文字）で、`KeyedDraws` はこれを受けます。`small` は i64 に入るときの値です。`WorldState.__setattr__` がスカラーを書くたびに `WorldCore.set_scalar` を呼び、`start_core` が始めに5つを読みます。`world.__dict__[name] = ...` のように `__setattr__` を通らない書き込みは見えません（テストの2か所がそう書きますが、kernel はまだ属性を読むので結果は変わりません）。kernel が属性を読むのはこの PR ではそのままで、読み出しを `World` に移すのは P8 です。
- **区域と区域の辺：** 実体の種類 6（`ZONE`）・7（`EDGE`）にし、中核の `Entities` に `zones`・`zone_edges` の表と、行の `ZoneRow`・`EdgeRow`（`EntityField` に `bool` を足しました）を置きました。設計は `core/src/state/zones.rs` としていましたが、`entity_row!` と表が `entities.rs` にあるので、同じファイルに置きました。`ZoneState`・`ZoneEdgeState` はほかの実体と同じ `__slots__`、`_kind`、`_install_rows` の書き込みの転送を持ち、`world.zones`・`world.zone_edges` は `IdMap` です（`ENTITY_FIELDS` に足しました）。挿入、削除、順、`is`、写しと pickle（素のオブジェクト）、`to_dict` は今と同じです。taint は人の5つの表と別で（`Entities::taint_zones`、`EntityStore.zones_tainted`）、区域の値が合わなくても人の行は残ります。`follows`、`rows_json`、`with_rows` は人の表だけを見ます（区域の行を読む kernel はまだありません）。`verify` と `refit` は7つを見ます。`install` は2つのクラスを多く受けます。
- **`resources` と `id_counters`：** 持つ項目（`Place::Field`）にしました。`OWNED_FIELDS` の行の型は、窓口の新しいモジュール `state/world_fields.rs` の `RESOURCES`（`IdMap<i64>`）と `ID_COUNTERS`（`ValueRow`。i64 を超える数も入ります）です。どちらも `TrackedDict` の木になり、Python の書き込みは行へ届きます。丸ごとの代入（`world.resources = {...}`。`from_scenario` が書きます）は `WHOLE_FIELDS` として `__setattr__` が捕まえ、新しい木にします（`metric_state`、`model_configs` は今までどおり捕まえません）。辞書でない値はそのまま置き、行を捨てます。Checkpoint の保存は、ほかの持つ項目と同じく行の bytes を使います（同じ bytes です）。
- **Rust の書き込み：** `rules/resources.rs` の新しい `resources` の辞書は、`replace_resources` で項目の新しい木にします（古い辞書を持つ人は古い値を持ち続けます）。`EventWriter::of` は、`id_counters` を Python の辞書ではなく行から読み、書くときは行、次に木へ書きます（`write_id_counter`）。行がなければ前と同じ文 `id_counters is not a plain dict` の `Unsupported` です。`rules/service.rs` の `resource_task` の3か所は `set_id_counter` を通します。木の外の辞書（`__setattr__` を通らずに置かれたもの）には前と同じ素の書き込みをします。
- **中核のテスト：** `native/core/tests/world.rs` に `Seed`（10進の文字だけ、i64 を超える値、`-0` や先頭の 0 を断る）、`Fit` と misfit の `Unsupported` の文、部分を借りたままのスカラーの読み出し、`id_counter`・`set_id_counter`（新しいキーは最後）、slot からの `resources` と `id_counters` を足しました。`native/core/tests/entities.rs` に、区域と辺の行の JSON のバイト列（期待値は CPython 3.12 の `WorldState._canonical_text(zone.to_dict())`）、種類と表、taint が互いに残ることを足しました。
- **Python のテスト：** `tests/integration/test_entity_rows.py` に、区域と辺の `IdMap` と行、書き込み、挿入・削除・置き換え・丸ごとの代入の順と `is`、区域だけの taint と `refit`、bool と int の取り違え、写しと pickle、文書が同じこと。`tests/integration/test_world_core.py` に、スカラーの `setattr`（大きな seed、型の違う値は misfit で、属性はそのまま）、写した世界のスカラー、`resources`・`id_counters` の木と行、丸ごとの代入、Rust の writer の書き込みが転送されないこと。`tests/integration/test_resource_ledger_row.py` で、新しい `resources` が行の木であることを確かめます。
- **確かめたこと：** PR 5-3 の後の `main`（`ce4c57b`）の上で、`cargo test -p sheltersim_core`（50件）、`cargo clippy --workspace -- -A clippy::all -D clippy::disallowed_methods`、`cargo tree -p sheltersim_core -e normal,build` に `pyo3` がないこと、`ruff check .`、`ruff format --check .`、offline の pytest（`-m "not long_simulation"`：5143 passed、6 skipped。`native/pylib` にこのビルドを置きました）、全経路のゲート（55 scenarios、0 problems）。
- **7日分**（100人、project の経路、生成 Scenario）：PR 5-1 の後の `main`（`21b50df`）のビルドと交互に2回ずつ測りました。world、events、metrics のハッシュは同じです。wall は base 73.0 s・66.3 s、この PR 72.1 s・64.8 s、thread は 68.0 s・62.1 s と 68.0 s・61.2 s、`TickEngine.step` の thread は 18.0 s・17.2 s と 18.0 s・17.1 s で、差は揺れの中です。計算のピークの RSS（`computation_mib`）は 893.5・891.2 MiB と 893.4・886.9 MiB で、増えていません。ほかのレーンの計算と重なった最初の2回ずつ（load average 約10）は、この PR が thread で +8%・+25% でしたが、変えていない計時（`FrameCache.put`、`ProjectStore.usage`）も同じ割合で遅く、機械の混み方の差でした。間に library の経路の2日分を交互に4回ずつ測り、`TickEngine.step` の thread は base 4.15〜4.32 s、この PR 4.20〜4.33 s で同じでした。`ce4c57b` の上に載せ直した後にも1回測り、3つのハッシュは同じでした（wall 65.1 s、thread 60.6 s、`computation_mib` 889.9 MiB）。

### PR 7-1：設定の解釈を stamp ごとに持つ（レーン 7-1）

- **中核の型**（`native/core/src/config/`）：`Configs` は `model_configs` の1つの状態から解釈した節を持ちます。今は選ばれたモデル（`Selection`、領域からモデルの ID への dict の順の対）と、tick の終わりの検査の plan（窓口の Python のオブジェクトを `Opaque` として持ちます）の2つです。節は `Lazy`（`OnceLock`）で、最初に読むときに解釈し、`Section`（`Absent`、値、`Fault`）を持ちます。`Fault` は誤りのクラス（`Value`、`Type`、`Key`、pydantic の `Validation`、読み手の断り `Refused`、ほかは名前）と `str(e)` です。`World` に状態でない部分 `derived: Shard<Derived>` を足しました。`Derived` は stamp の新しい2つの `Configs` を持ちます（取り消しで古い stamp が戻っても当たります）。stamp は 5-1 のプロセスで1つのカウンターのものを使います。
- **窓口**（`native/src/core/configs.rs`）：`current(world)` が `model_configs` の slot の stamp（`world_core.rs` の `field_stamp`）で `Configs` を引き、節は今の Python の契約（`selected_models`、`tick_plan`）を同じ木に呼んで埋めます。解釈は例外を出しません。読む場所は今の断り（U）の文をその行で出し、そのまま伝える誤り（R、P）は2回目の解釈の誤りを出します（方針 E14、`reraise`）。同じ設定の解釈は同じ値か同じ誤りなので、誤りのクラス、文、出る時点は変わりません。stamp のない設定（core のない世界、丸ごと代入した項目）と、テストが差し替えた `selected_models` は、読むたびに解釈します（前と同じ）。
- **読む場所**：設計の表の #4〜4e（`body_state`、`information_events`、`queues`、`support`、`world_view`）は `Selection` を読みます。#25 の `after_tick` は plan を `Configs.tick_plan` から読み、`TickMemo` は plan を読んだ stamp だけを持ちます（I8）。新しい memo（失敗の後、時刻が戻ったとき）も、同じ stamp なら `tick_plan` をもう一度呼びません。`world_view` の `call_failed` と、`selected_models` を一度だけ引いて持っていた3つの `GILOnceCell` は消しました。テスト用に `information_events.selected_information_of` を足しました。
- **決めたこと**：
  - 選ばれたモデルの値は str だけにしました（`_select_models` は str だけを返します）。dict でない、または str でない値の選択は、読み手の断り（`not a selection`）です。差し替えたテストの関数でしか起きません。
  - 節は読むときに解釈します。設計 §3 の「内容と順が同じ節は `Arc` を残す」比べ方は入れていません。今は写して差し替えると新しい core（空の `Derived`）になり、同じ World の中で stamp が変わるのは本当に書いたときだけなので、比べても当たりません。トランザクションで World が残るようになる 6-2・6-3 で足します。
  - 設計の `raw`、`root` の欄は、それを読む節（7-5a の #12、#24）と一緒に足します。
  - **U1**（その場の書き込みは次に読むときに見える）：全経路のゲートは各 Scenario のプロセスで `watch_config_writes(True)` を呼び、tick の phase の中（`_SELECTION_SCOPE` があるとき）で `model_configs` の木に Python から書いた数を数えます（`NamespaceMirror::apply` の入口）。1以上はゲートの problem です。55の Scenario で0でした。
- **中核のテスト**（`native/core/tests/config.rs`、7件）：同じ stamp は同じ `Configs`、2つまで残すことと取り消しの後に当たること、節を1回だけ解釈すること、`Fault` のクラスと文、解釈の中の解釈、`Selection` の順、取り消しの後に書くと新しい stamp で解釈し直すこと。
- **offline のテスト**（`tests/integration/test_config_cache.py`、7件）：1つの stamp で1回だけ解釈し、読んでも stamp は変わらないこと、その場で書くと次に読むときに解釈し直すこと、R の読む場所は `selected_models` と同じクラスと文の誤りを、U の読む場所は断りを出すこと、stamp のない設定と差し替えた関数は読むたびに解釈すること、plan が設定と一緒に残り、誤る plan は毎回同じ誤りになること、phase の中のその場の書き込みが数えられ、2時間の Run では0であること。
- **確かめたこと**（PR 5-3 の後の `main`（`ce4c57b`）に載せ直して）：`cargo test -p sheltersim_core`（49件）、`cargo clippy --workspace -- -A clippy::all -D clippy::disallowed_methods`、`cargo tree -p sheltersim_core -e normal,build` に `pyo3` がないこと、`ruff check .`、`ruff format --check .`、offline の pytest（`-m "not long_simulation"`：5129 passed、6 skipped）、全経路のゲート（55 scenarios、0 problems）、7日分を1回（ハッシュは下の base と同じ）。下の時間は、載せ直す前（5-1 の後）に測ったものです。
- **ゲートの時間**（`--jobs 8`、5-1 のビルドと交互に2回ずつ）：全体は base 42.2 s・34.7 s、この PR 33.1 s・34.5 s、Scenario ごとの wall の合計は 150.8 s・135.9 s と 134.6 s・139.6 s です（1回目の base はキャッシュが冷えていました）。トランザクションごとの解釈し直しで、目に見えて増えてはいません。
- **7日分**（100人、project の経路、生成 Scenario）：5-1（`main` の `21b50df` と同じ木）のビルドと交互に測りました。world、events、metrics のハッシュは同じです。ほかのレーンが同時に動いていて揺れが大きいので、4回ずつ測りました（前の2組は負荷が高いとき、後の2組は低いとき）。wall は base 91.7・87.4・69.0・72.9 s、この PR 78.2・93.7・66.1・71.9 s、thread は 78.1・77.7・64.2・67.9 s と 72.5・86.9・61.9・67.8 s、`TickEngine.step` の thread は 20.2・20.3・17.5・17.9 s と 19.5・22.8・17.0・17.9 s です。負荷の低い2組では thread が -3.6% と -0.1% で、設計の見込み（0.5〜1.5%）どおり、揺れの中の小さな差です。計算のピークの RSS（`computation_mib`）は 890.2・884.2・890.1・883.9 MiB と 898.0・901.6・889.5・898.6 MiB で、5-1 で測った幅（888〜899 MiB）の中です。

### この後

残りの PR は、下の「レーン」の順に進めます。各レーンは、終わったときにこの節の前に記録を足します。
