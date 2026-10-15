# pbt の ControlMessage 生成に Unsupported を追加する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/test-pbt-unsupported-control-message
- Polished: {YYYY-MM-DD}

## 目的

`ControlMessage::Unsupported` の encode / decode ラウンドトリップを PBT で守る。この variant は本体を生バイト列で保持し、型 ID によって Request ID の有無が変わるため、encode と decode の対応が崩れても単体テストでは気づきにくい。

## 現状

- `pbt/tests/prop_message.rs` の `sample_control_message` は 15 種の ControlMessage を生成するが、`ControlMessage::Unsupported` を含まない。
- そのため `Unsupported` の encode → decode 往復 (Type + Length の再構成、`request_id` の再抽出、body の保持) は PBT で検証されていない。
- 任意入力に対するクラッシュ耐性は `fuzz/fuzz_targets/fuzz_session.rs` の `Op::RecvRequest` 経由の `Session::recv_request` が部分的に担保しているが、ラウンドトリップの一致は検証していない。
- 未対応 request 3 種は `Session::recv_request` が本体を解析するようになったため、body の保持が崩れると NOT_SUPPORTED の拒否経路まで到達しなくなる。

## 設計方針

- `sample_control_message` に `ControlMessage::Unsupported` を追加する。draft-ietf-moq-transport-22 §9 Table 5 の 6 型のうち、request として届く 3 種 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) と応答専用の 3 種 (NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED) を生成し、`request_id` は型に応じて `Some` / `None` を選ぶ。
- body は生バイト列としてサンプル生成し、decode 側が要求する構造を満たすようにする。request 型は `ControlMessage::decode` が本体の先頭 varint を Request ID として読むため、非空で varint としてデコードできるバイト列にする。
- 往復で `type_id` / `request_id` / `body` が一致することを検証する。decode が失敗する入力は生成側で排除し、「decode できたときだけ検証する」形にしない。

## 完了条件

- `pbt/tests/prop_message.rs` の ControlMessage ラウンドトリップ PBT が `Unsupported` を生成し、`type_id` / `request_id` / `body` の一致を検証していること。
- `cargo test --workspace` (pbt を含む) / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
