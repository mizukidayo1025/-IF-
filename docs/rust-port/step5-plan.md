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
- **E3. 中核の `World`：** 1つの世界の状態を、1つの Rust の構造体にまとめます。窓口の `WorldCore` がそれを持ち、kernel は `&mut World` を受けます。実体、区域、名前空間の木などの Python のオブジェクトは、`World` を読み書きする窓（view）にします。実体の辞書は挿入順を保ちます（`IndexMap`、`shift_remove`）。
- **E4. Host を呼ぶ関数は、世界を借りたままにしません。** 中核の関数は `WorldCell`（`with(|world| ...)` で短く借りる）を受け、Host を呼ぶ間は借りません。今の「Python を呼ぶ間は borrow を持たない」と同じ規則です。そのためネイティブ版の Host（LLM の境界）は、今までどおり Python の view を通して書けます。
- **E5. 写して差し替える処理は、`World` の fork と commit にします。** EventLog は追記だけなので、fork では長さの印を持ち、写しません。
- **E6. 候補の世界の丸ごとの検証**（`WorldState.validate`、`validate_simulator_extensions`）は、#152 では Host の `validate_candidate` にします。ネイティブ版は、候補の Python の view に今の Python の検証をかけます（手順4の D3「`validate_*` は Python に残す」と同じ考えです）。検証の文を中核で作るのは段階4に回します。`after_tick.rs`・`tick_validation.rs` の「問題なし」と答える検査は、中核に移します。
- **E7. 設定：** 窓口が、Run や Branch の始まりと `model_configs` が変わったときに設定を解釈し、型の決まった中核の `Config` を渡します。設定の誤りの文が出る時点は変えません。
- **E8. 断り：** `Unsupported` を返す約240か所は、中核の `Error::Unsupported` にします。窓口で今と同じ `Unsupported`、`KernelRefusal` になります。
- **E9. 記録の bytes は中核が持ちます。** Python の `bytes` は、要るときに作ってキャッシュし、保存のキャッシュが頼る「同じオブジェクト」を保ちます。そのレーンで計算のピークのメモリーを測り、増やしません（#138）。
- **E10. 窓口に残すもの：** tick への patch や shadow の検出、断りの数の統計、`rules/parity.rs`（テスト用）。
- **E11. 残る Python の規則：** 通常の経路の4つは Rust に移します。届かない fallback は消し、消したテストを PR に挙げます。`upgrade_information_runtime_state` は tick ではなく復元の側（中核の外）に移します。
- **E12. 実体の view は書き込みもできる形にします。** 約69のテストのファイルが、そのまま動くようにするためです。`vars(agent.needs)`、dataclass の `==`、enum の `is` を保ちます。

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

### この後

残りの PR は、下の「レーン」の順に進めます。各レーンは、終わったときにこの節の前に記録を足します。
