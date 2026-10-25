# pbt の ControlMessage 生成に Unsupported を追加する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/test-pbt-unsupported-control-message
- Polished: 2026-10-25

## 目的

`ControlMessage::Unsupported` の encode / decode ラウンドトリップを PBT で守る。この variant は本体を生バイト列で保持し、型 ID によって Request ID の有無が変わるため、encode と decode の対応が崩れても固定値の単体テストだけでは入力の組み合わせの取りこぼしに気づきにくい。

## 現状

- `pbt/tests/prop_message.rs` の `sample_control_message` は 15 個の重み付きインデックスを持つが、`ControlMessage::Unsupported` の分岐を含まない。
- そのため `Unsupported` の encode → decode 往復 (Type + Length の再構成、`request_id` の再抽出、body の保持) は PBT で検証されていない。
- 任意入力に対するクラッシュ耐性は `fuzz/fuzz_targets/fuzz_session.rs` の `Op::RecvRequest` 経由の `Session::recv_request` が部分的に担保しているが、ラウンドトリップの一致は検証していない。
- 未対応 request 3 種は `Session::recv_request` が本体を解析するようになったため、body の保持が崩れると NOT_SUPPORTED の拒否経路まで到達しなくなる。

## 設計方針

- `sample_control_message` に `ControlMessage::Unsupported` を追加する。draft-ietf-moq-transport-22 §9 Table 5 のうち本ライブラリが実装しない 6 型を対象にする。
- 生成する型は request として届く 3 種 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS、型 ID 0x06 / 0x50 / 0x51) と応答専用の 3 種 (NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED、型 ID 0x08 / 0x0E / 0x0F) である。`request_id` は型に応じて `Some` / `None` を設定する。
- `UNSUPPORTED_MESSAGE_TYPES` は `pub(crate)` のため、PBT 側では型 ID を直接指定する。
- body は生バイト列としてサンプル生成する。request 型は `ControlMessage::decode` が本体の先頭 varint を Request ID として読むため、非空で先頭 varint が `request_id` と一致するバイト列にする (`varint::encode` で `request_id` を body の先頭に書き、その後ろは任意のバイト列でよい)。
- 応答専用型は decode が本体を解析しないため、任意のバイト列 (空を含む) でよい。
- body 全体の構造 (Track Namespace Prefix や Parameters) は `ControlMessage` の encode / decode では検証されない。本 PBT は `ControlMessage::encode` / `ControlMessage::decode` の往復だけを対象とし、`Session::recv_request` の本体解析は対象外である。
- 0317 (`issues/0317-bug-unsupported-message-encode-validation.md`) で `ControlMessage::Unsupported` の encode に検証が追加される予定である。request 型では `request_id` と body 先頭 varint を一致させ、`type_id` が上記 6 型に含まれ、`request_id` の有無が型と一致していることを生成側で保証する。これにより 0317 の実装前後どちらでも encode が成功する。
- `sample_control_message` の doc コメントと `noprop::sample_weighted_index` の重み配列を 16 個の分岐に更新する。重み配列を 15 個のままにすると、追加した 16 番目の分岐が選ばれず `Unsupported` が生成されない。
- 往復で `type_id` / `request_id` / `body` が一致することを検証する。encode または decode が失敗する入力は生成側で排除し、「decode できたときだけ検証する」形にしない。既存の `roundtrip` は `assert_eq!(decoded, msg)` で全フィールド一致を検証しているため、個別のフィールド比較を重ねない。
- `roundtrip` に `unsupported_seen` のゲートを追加する。既存の `fetch_seen` / `notify_seen` と同様に、少なくとも 1 件の `Unsupported` が往復を通って観測されることを固定し、重み付き抽選に依存して `Unsupported` が一度も選ばれない状態を許さない。`roundtrip` のコメントも観測対象に合わせて更新する。

## 完了条件

- `pbt/tests/prop_message.rs` の `sample_control_message` が `Unsupported` の 6 型すべてを生成する分岐を持ち、重み配列と doc コメントが 16 個の分岐に更新されていること。
- `pbt/tests/prop_message.rs` の ControlMessage ラウンドトリップ PBT が `Unsupported` を生成し、`type_id` / `request_id` / `body` の一致を検証していること。`unsupported_seen` ゲートにより、少なくとも 1 件の `Unsupported` が往復を通って観測されること。
- request 型の `request_id` と body 先頭 varint が一致し、`type_id` と `request_id` の有無が 0317 の encode 検証と整合していること。
- `cargo test --workspace` (pbt を含む) / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
