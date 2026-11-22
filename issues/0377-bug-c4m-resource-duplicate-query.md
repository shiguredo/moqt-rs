# resource の重複した tns / tn をすべて検証する

- Created: 2026-10-25
- Completed: 2026-11-22
- Branch: feature/fix-c4m-resource-duplicate-query
- Polished: 2026-10-25

## 目的

`AuthorizationContext::verify_resource_consistency` は、`resource` のクエリに重複した `tns` / `tn` がある場合に最後の値だけを使う。draft-ietf-moq-c4m-01 §3.1.2 は `resource` が `tns` / `tn` と整合することを MUST とし、各クエリパラメータが actx の対応フィールドと一致することを求める。重複した値のうち最後の値だけが一致する `resource` を受理すると、仕様の MUST を満たさない。

## 現状

- `src/c4m/dpop.rs` の `AuthorizationContext::verify_resource_consistency` は、クエリを `split('&')` で分割し、`tns` / `tn` の値を `Option<&str>` に上書きして最後の値だけを保持する。`tns` と `tn` の比較も最後の値だけで行う。
- `tests/test_c4m/dpop.rs` の `resource_consistency_errors` は重複したクエリパラメータを固定していない。
- `pbt/tests/prop_c4m/dpop.rs` の `resource_consistent` も最後の値を使う写経であり、重複した値のすべてを比較していない。PBT モデルの独立化は 0356 で扱う。

## 設計方針

- `AuthorizationContext::verify_resource_consistency` は、クエリの `tns` / `tn` の値をすべて収集し、比較対象のパラメータについて、すべての値が actx の対応フィールドと一致する場合だけ `Ok(())` を返す。1 つでも一致しない値があれば `DpopError::ResourceInconsistent` を返す。
  比較対象のパラメータがクエリに無い場合、`tns` は一致しないものとして `DpopError::ResourceInconsistent` を返し、namespace 操作の `tn` は比較を省略し、track 操作で actx に `tn` がある場合の `tn` は一致しないものとして `DpopError::ResourceInconsistent` を返す。
- 0346 の action 別検証の規則を維持する。
  - SETUP: クエリの `tns` / `tn` を比較しない。
  - namespace 操作: `tns` を比較し、actx に `tn` がありクエリに `tn` がある場合だけ `tn` を比較する。
  - track 操作: `tns` を比較し、actx に `tn` がある場合は `tn` も比較する。
  - 比較対象のパラメータに重複がある場合は、すべての値の一致を要求する。
- 0346 の完了後に着手する。0346 が `verify_resource_consistency` のシグネチャと action 別の分岐を変えるため、その変更を取り込んだうえで重複の検証を追加する。
- `tests/test_c4m/dpop.rs` に、同じ値の重複 (整合)、異なる値の重複 (不一致)、重複した `tn` の不一致、`resource` に `tns` / `tn` が無い場合のテストを追加する。
  異なる値の重複と重複した `tn` の不一致は、最後の値だけが actx と一致するケースを含め、修正前の実装が受理することを確認できるようにする。SETUP 以外で `tns` が無い場合と、track 操作で actx に `tn` がありクエリに `tn` が無い場合は `DpopError::ResourceInconsistent`、namespace 操作でクエリに `tn` が無い場合は整合になることを固定する。
- `pbt/tests/prop_c4m/dpop.rs` のモデルは 0356 で独立化する。本 issue では実装と単体テストを修正し、PBT のモデルは変更しない (0356 のモデルが同じ規則で判定することを前提とする)。

## 変更対象

- `src/c4m/dpop.rs`
- `tests/test_c4m/dpop.rs`

## 完了条件

- `verify_resource_consistency` が、比較対象の `tns` / `tn` に重複がある場合、すべての値が actx と一致するときだけ `Ok(())` を返し、1 つでも一致しない値があるときは `DpopError::ResourceInconsistent` を返すこと。
- SETUP / namespace 操作 / track 操作の action 別の比較対象が 0346 の規則どおりであること。SETUP 以外でクエリに `tns` が無い場合と、track 操作で actx に `tn` がありクエリに `tn` が無い場合は `DpopError::ResourceInconsistent` を返し、namespace 操作でクエリに `tn` が無い場合は比較を省略すること。
- `tests/test_c4m/dpop.rs` に上記のテストが追加され、`cargo test --workspace` が通ること。
- `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `markdownlint-cli2` が変更した Markdown で通ること。

## 解決方法

- `src/c4m/dpop.rs` の `resource_query_parameters` を `resource_query_parameter_values` に置き換え、クエリの `tns` / `tn` を出現順にすべて返すようにした。パーセントエンコーディングを解釈しない点は変えていない。
- `resource_values_match` を追加し、値が 1 つ以上あり、すべて期待値と一致する場合だけ整合とする。比較対象のパラメータがクエリに無い場合は一致しない。
- namespace 操作は `tns` のすべての値を比較し、actx とクエリの両方に `tn` がある場合だけ `tn` のすべての値を比較する。クエリに `tn` が無い場合は比較を省略する。
- track 操作は `tns` のすべての値を比較し、actx に `tn` があればクエリの `tn` のすべての値を比較する。クエリに `tn` が無い場合も `DpopError::ResourceInconsistent` を返す。actx に `tn` が無い場合は比較せず、後段の `verify_target` が `MissingTrackName` で拒否する。
- SETUP は従来どおり `tns` / `tn` を比較しない。0346 の action 別検証の規則は維持している。
- `tests/test_c4m/dpop.rs` に `resource_consistency_checks_every_duplicate_parameter` を追加した。同じ値の重複の受理、最後の値だけが一致する `tns` / `tn` の重複の拒否、`tns` が無い場合、track 操作で `tn` が無い場合、actx に `tn` が無い場合の比較省略を確認する。
- `pbt/tests/prop_c4m/dpop.rs` のモデルは 0356 の範囲のため変更していない。
- Lean 検証を `shiguredo/lean` の `moqt-rs/C4mResource/` に追加した。`allEq` が空でないすべての値の一致を要求すること、重複をすべて保持すること、action 別の比較対象と、最後の値だけが一致する重複の拒否を 12 個の定理と 9 個の有限例で確認した。

確認コマンド: `cargo test --workspace`、
`cargo test -p shiguredo_moqt --features aws-lc-rs`、
`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo clippy -p shiguredo_moqt --all-targets --features aws-lc-rs -- -D warnings`、
`cargo fmt --all -- --check`、
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ることを確認した。
