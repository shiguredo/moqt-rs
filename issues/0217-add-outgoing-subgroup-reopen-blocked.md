# STOP_SENDING による再オープン禁止を送信前に判定できるようにする

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-outgoing-subgroup-reopen-blocked
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §11.3.2 (Closing Subgroup Streams) は、STOP_SENDING を受けた Subgroup の
再オープンを Forward State 0→1 の REQUEST_UPDATE 受理後だけに許す。Session は禁止状態を保持し、
`Session::send_subgroup_header` と `Session::send_subgroup_object` (FirstObjectId の初回 Object) が
`SESSION_PROTOCOL_VIOLATION` を返して拒否する。一方でアプリが送信前に可否を判定する公開 API が無いため、
example は「送って拒否されたら group id を進める」形で回避しており、Session の保持する状態と example の
自前管理が二重になっている。送信前に判定できる API を用意し、example が Session の状態に基づいて
subgroup / group を選べるようにする。

## 現状

- `src/session/data.rs` の `Session::outgoing_subgroup_reopen_blocked` は private で、
  `send_subgroup_header` と `send_subgroup_object` から呼ばれる。
- 公開 API は `Session::stopped_outgoing_subgroup_error_code` のみで、記録時のキーと完全一致する
  `(request_id, track_alias, group_id, subgroup_id)` だけを返す。doc には「再オープン禁止の判定
  (`outgoing_subgroup_reopen_blocked`) は alias を共有する別 request のエントリも照合するため、本 API が
  `None` を返しても再オープンが禁止されている場合がある」と明記されている。
- 禁止エントリは Forward State 0→1 の REQUEST_UPDATE 受理、または `Session::forget_subscription` で
  破棄される。
- `examples/moq-pub/src/pipeline.rs` は STOP_SENDING を受けた group を再送しないよう、自前で
  `video_group_id` / `audio_group_id` を進めている。

## 設計方針

- 送信前に再オープン禁止を判定する公開 API を追加する (例:
  `Session::outgoing_subgroup_reopen_blocked(&self, track_alias: u64, group_id: u64, subgroup_id: Option<u64>) -> bool`)。
  `subgroup_id` の `None` は FirstObjectId モードの未解決 subgroup を表す。
- 既存の `stopped_outgoing_subgroup_error_code` との使い分け (コードを取得する / 可否だけを判定する) を
  doc に書く。alias を共有する場合の判定範囲が内部判定と一致することを明記する。
- example の自前管理を判定 API に置き換えるかは、同じ挙動を保てるかを確認してから決める。§11.3.2 は
  Forward State 0→1 の REQUEST_UPDATE 受理後に再オープンを許すため、受理後の再開も判定できる必要がある。
- 送信時の拒否 (`SESSION_PROTOCOL_VIOLATION`) は防御として残す。

## 完了条件

- 送信前に再オープン禁止を判定できる公開 API があること。
- alias を共有する別 request のエントリも含めて内部判定と一致することを単体テストで固定すること。
- 既存の `stopped_outgoing_subgroup_error_code` の挙動が変わらないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
