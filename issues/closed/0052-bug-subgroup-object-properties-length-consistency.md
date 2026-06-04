# encode の properties blob の Properties Length と実データ長の一致を検証する

- Created: 2026-09-10
- Completed: 2026-09-11
- Branch: feature/fix-properties-length-consistency
- Polished: 2026-09-11

## 目的

`SubgroupObject::encode` / `FetchStreamObject::encode` が、Properties Length が実際のデータ長と一致しない `properties_data` を素通しして不正ワイヤを生成するのを防ぐ。

## 現状

0033 で `SubgroupObject::encode` は空スライス `Some(&[])` を拒否するようになり、0051 で `FetchStreamObject::encode` も同様になったが、Properties Length と実データ長の整合性はどちらも検証されない。

- Normal status (`None` / `Some(0)`) の `SubgroupObject::encode` は Properties の内容を検査しない。`Some(&[0x80])` (2 バイト必要な Length varint が 1 バイトで途中)、`Some(&[0x40])` (Length = 64 を宣言して後続データ 0 バイト)、`Some(&[0x00, 0xAA])` (Length = 0 を宣言して余分な 1 バイト) がそのまま `buf` に書かれて `Ok` を返す。
- 非 Normal status では既存検査が Length varint を decode して `prop_len > 0` を拒否するため、`Some(&[0x80])` と `Some(&[0x40])` は拒否される。ただし `Some(&[0x00, 0xAA])` は `prop_len == 0` のためすり抜ける。
- `FetchStreamObject::encode` は status を持たないため、上記のすべてが検証されない。

これらの不正 blob が書き込まれると、decoder は宣言長の分だけ後続バイトを Properties として消費し、後続フィールドの境界がずれて payload 長などを誤読する。

根拠: draft-ietf-moq-transport-21 §11.1.3 (Object Properties): `Properties Length (vi64)` + Properties の組で、Length は後続バイト数を表す。§8.1 (Variable-Length Integers): `0x40` は先頭ビットが `0` の 1 バイト vi64 (値 64) であり、途中で切れた varint の例にはならない。

## 設計方針

書き込み開始前に `varint::decode(properties_data)` で Length varint を読み、消費バイト数 `n` と `properties_data.len()` から整合を検証する。`n + Length` の加算形は `Length = u64::MAX` (9 バイト varint) で u64 の桁あふれを起こすため、`Length == (properties_data.len() - n) as u64` の減算形、または `varint::checked_len` の再利用で桁あふれを避ける。
status の Normal / 非 Normal を問わず行い、`Length = 0` + 余分なバイトも拒否する。不一致・varint 不正は `ProtocolViolation` とする。

## 完了条件

- 途中で切れた Length varint (`&[0x80]` 等) と、Properties Length が実データ長と一致しない `properties_data` (`&[0x40]` の Length = 64 + データ 0 バイト、`&[0x00, 0xAA]` の Length = 0 + 余分なバイト等) が `ProtocolViolation` で拒否されること
- Length が `u64::MAX` の 9 バイト varint (`&[0xFF; 9]` 等) でも、桁あふれで panic せず `ProtocolViolation` になること
- Properties Length = 0 を含むデータ (`&[0x00]`) と、Length が一致する非空プロパティが正常にエンコードされること
- `SubgroupObject::encode` / `FetchStreamObject::encode` の両方に適用されていること
- 回帰テストが `tests/test_stream/subgroup_object.rs` / `tests/test_stream/fetch_stream_object.rs` 等に追加されていること

## 解決方法

`SubgroupObject::encode` / `FetchStreamObject::encode` に Properties Length と実データ長の一致検証を追加し、不正ワイヤの生成を防いだ。

- `src/stream.rs` に `validate_properties_blob` を追加し、`varint::decode` の消費バイト数 `n` を使って `Length == (data.len() - n)` を検証する (加算形は `u64::MAX` で桁あふれするため減算形)。
- `SubgroupObject::encode` は書き込み前に同ヘルパーを呼び、非 Normal status の検査も同ヘルパーの戻り値に統合した。`FetchStreamObject::encode` も書き込み前に呼ぶ。
- 途中で切れた Length varint、Length と実データ長の不一致、`u64::MAX` の 9 バイト varint を `ProtocolViolation` で拒否する回帰テストと、非最小エンコーディング受理・非空 Properties の正常系テストを追加した。
- `CHANGES.md` の `[FIX]` にエントリを追加した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / no_std ビルドが通ることを確認した。
