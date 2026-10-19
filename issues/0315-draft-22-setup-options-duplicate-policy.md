# SETUP Options の重複 Option Type ポリシーをテストで固定する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/test-setup-options-duplicate-policy
- Polished: 2026-10-20

## 目的

draft-ietf-moq-transport-22 §9.1 (SETUP) は「Senders MUST NOT repeat the same Option Type in a message unless the option definition explicitly allows multiple instances.」とし、受信側に「Receivers MUST allow duplicates of unknown Setup Options.」を課す。
`SetupOptions` の encode と decode は、送信側の重複拒否、受信側の既知型重複拒否、未知型重複受理を実装しているが、テストが無く、規約違反を検出できない。

## 現状

- `src/parameter.rs` の `SetupOptions::encode` は AUTHORIZATION_TOKEN 以外の同一 Option Type の重複を `InvalidParameter` にする。
- `src/parameter.rs` の `SetupOptions::decode` は AUTHORIZATION_TOKEN 以外の既知型の重複を `ProtocolViolation` にし、未知型の重複は受理する。
  - AUTHORIZATION_TOKEN (0x03) は既知型だが重複が許可されており、この重複判定の対象外である (`known_types` に含まれない)。
- `src/parameter.rs` の `SetupOptions::encode` / `decode` は `validate_auth_token_uniqueness` により、AUTHORIZATION_TOKEN の同一 (Token Type, Token Value) の重複を `MalformedAuthToken` にする。
- `tests/test_parameter.rs` と `pbt/tests/prop_parameter.rs` に重複を扱うテストが無い。PBT の `sample_setup_options_with` は `dedup_by_key(|p| p.option_type)` で型を dedup するため、この分岐を通らない。
- 影響: §9.1 の送信側 MUST と未知型の受信側 MUST が未固定で、将来の変更で規約違反を検出できない。

## 設計方針

- (a) 既知型 0x01 (PATH、奇数型) の `SetupOptionValue::Bytes` 2 件を encode すると `InvalidParameter` になること。
- (b) 既知型 0x05 (AUTHORITY、奇数型) の同じ型の 2 件を decode すると `ProtocolViolation` になること。
  - decode は encode で重複を作れないため、delta=0 と長さ付きバイト列を含む手組みのワイヤで入力する。
- (c) §16.4 (Setup Options) のレジストリ表に無い未知型 0x09 の重複を decode すると受理され、`consumed == buf.len()` かつ 2 件とも保持されること (`len() == 2`)。
  - 0x09 は Message Parameter では LARGEST_OBJECT だが、Setup Options は別名前空間なので未知型として扱う。
- (d) §9.1.4 (AUTHORIZATION TOKEN) は「The endpoint can specify one or more tokens in SETUP」と定めており、異なる (Token Type, Token Value) を持つ 2 つの `AuthorizationToken::UseValue` の重複が encode / decode とも許可されること。
  - decode では `consumed == buf.len()` かつ 2 件とも `authorization_tokens()` に含まれること。
  - §8.9 (Authorization Token Compression) は「An Authorization Token MAY be repeated within a message as long as the combination of Token Type and Token Value are unique after resolving any aliases.」と定めるため、同一 (Token Type, Token Value) の重複は `MalformedAuthToken` になり、このケースでは使わない。
- draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) の偶数型は varint、奇数型は長さ付きバイト列という規則に反する値形式を encode すると `InvalidParameter` になること。even 型に `Bytes`、odd 型に `VarInt` を入れ、重複チェックで先に落ちないよう 1 件だけの `SetupOptions` で検証する。
- 既知型の定義は draft-ietf-moq-transport-22 §16.4 (Setup Options) のレジストリ表に基づく。
- テストは `SetupOptions::encode` / `decode` の公開 API 経由で書き、`tests/test_parameter.rs` に追加する。
- PBT の `sample_setup_options_with` は重複を生成しない仕様のまま変更しない。重複の分岐は `tests/test_parameter.rs` の単体テストで固定する。
- `src/parameter.rs` は変更しない。テストが現行実装と食い違う場合は、issue の前提が誤っているため作業を止めて報告する。

## 完了条件

- 上記 (a)〜(d) と型不一致のテストが `tests/test_parameter.rs` にあり、すべて通ること。
- 各テストは対象の分岐だけを検証すること。0x01 / 0x05 / 0x09 の重複テストは奇数型の `SetupOptionValue::Bytes` を使い、AUTHORIZATION_TOKEN は `SetupOptionValue::AuthorizationToken` を使う。型不一致テストは 1 件だけの `SetupOptions` を使うこと。
- decode の重複テストは、encode が重複を拒否するため、delta=0 を含む手組みのワイヤで入力すること。
- AUTHORIZATION_TOKEN の重複テストは、§8.9 の一意性条件を満たす異なる (Token Type, Token Value) を使うこと。
- PBT の `sample_setup_options_with` と既存のテストは変更しないこと。
- `src/parameter.rs` は変更しないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
