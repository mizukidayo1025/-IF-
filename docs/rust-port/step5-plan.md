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

### この後

棚卸しの結果と、残りの PR の順番は、次の PR でこの文書に書きます。
