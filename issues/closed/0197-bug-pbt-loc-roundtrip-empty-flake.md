# pbt の LOC ラウンドトリップがシードによって失敗するのを直す

- Created: 2026-10-03
- Completed: 2026-10-03
- Branch: feature/fix-pbt-loc-roundtrip-empty-flake
- Polished: {YYYY-MM-DD}

## 目的

`pbt/tests/prop_loc.rs` の `roundtrip_including_empty` が乱数シードによって稀に失敗し、CI が赤くなって無関係な変更のマージを止める。サンプリングの分布に依存しないテストへ直す。

## 現状

- テストは 256 ケースの PBT の後で「空の LOC プロパティのケースが 1 つ以上観測されたこと」を assert している
- 1 ケースが空 (プロパティ 0 個) になる確率は 1/32 (5 つの bool がすべて false)。256 ケースで 1 回も生成されない確率は `(31/32)^256 ≈ 0.029%` である
- 実測: CI が残したシード `0x18da512254f863c0` を指定するとローカルで再現する。シード 1 から 1000 までは再現しない (0/1000)
- `accessor_consistency` の 4 分岐の観測 assert は 1 ケースあたりの確率が 1/3 以上あり、256 ケースでの失敗確率は事実上無視できる (`(2/3)^256 ≈ 10^-45`)
- 失敗した CI は 0186 のブランチで発生したが、0186 の変更ファイルに `pbt/tests/prop_loc.rs` は含まれず、差分とは無関係である

## 設計方針

- 空のプロパティの encode → decode は、PBT のサンプリングに依存させず、テストの先頭で明示的に 1 回検証する。`LocProperties::new()` を encode すると Properties Length = 0 (varint の 1 バイト `0x00`) になり、decode で空に戻ることを確認する
- 空・非空の観測を数える `empty_seen` / `non_empty_seen` とその assert は削除する。任意のプロパティ列のラウンドトリップ検証は PBT が担う
- 他のテスト (`accessor_consistency` など) は変更しない

## 完了条件

- `roundtrip_including_empty` が、CI で失敗したシード `0x18da512254f863c0` と複数のシードで成功すること
- 空のプロパティの encode → decode がテストで必ず実行されること (サンプリングに依存しないこと)
- `cargo test -p pbt` と `cargo test --workspace` が通ること

## 解決方法

- `pbt/tests/prop_loc.rs` の `roundtrip_including_empty` から、空・非空の観測数を数える `empty_seen` / `non_empty_seen` とその assert を削除した
- 空の `LocProperties` の encode → decode をテストの先頭で明示的に検証するようにした (encode は varint 1 バイトの `0x00` になり、decode で空に戻る)。任意のプロパティ列のラウンドトリップは PBT が担う
- CI で失敗したシード `0x18da512254f863c0` とシード 1 から 1000 までで成功することを確認した。`accessor_consistency` の観測 assert は 1 ケースあたりの確率が 1/3 以上で、256 ケースでの失敗確率は事実上無視できるため変更していない
