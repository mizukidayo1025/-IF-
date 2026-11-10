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
- **E2. `V` を中心に分けます。** 中核の `Field` は `V` との行き来（`from_v`、`to_v`）と JSON の書き出しを持ち、Python との行き来は窓口の trait にします。まだ Python の木に書く setter は、中核の trait（`TreeSink`）を通します。
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

### この後

残りの PR は、下の「レーン」の順に進めます。各レーンは、終わったときにこの節の前に記録を足します。
