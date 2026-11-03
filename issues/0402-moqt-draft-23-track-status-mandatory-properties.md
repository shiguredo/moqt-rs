# TRACK_STATUS_OK の未知 Mandatory Track Property を検査する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.7、§9.3、§9.12](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/core.rs` の `handle_peer_request_ok` は Track Properties を TRACK_STATUS_OK で受理するが、`handle_ok_for_track_status` へはパラメータだけを渡す。
`src/session/track_status.rs` の同関数にも `has_unknown_mandatory` に相当する検証がなく、未対応の Track を通常の成功として処理できる。

## 設計方針

TRACK_STATUS_OK を処理する前に未知 Mandatory Track Property を確認し、当該 Track を利用可能な成功として処理しない。
`handle_peer_request_ok` から `handle_ok_for_track_status` へ Track Properties も渡す。
未知 Mandatory 型があれば、`TrackStatusResponse::Unsupported` を追加して応答受領済みのローカル未対応結果を記録する。
通常の `RequestOkReceived` は発行せず、専用の `SessionEvent::TrackStatusUnsupported` で Request ID と未対応 Property Type の一覧を通知する。
未対応結果でも応答タイマーを解除し、FIN と `forget_track_status` は応答受領済みの単発要求と同じ条件に従う。ローカル cancel 済みの遅延応答は既存の検証・吸収順序を維持し、新たな未対応通知を発行しない。
既に届いた OK に対して逆向きに REQUEST_ERROR を返さない。
draft が記載する downstream へのエラー伝播はリレー機能のため追加しない。

## 完了条件

- 通常の Track Properties は成功として扱い、未知 Mandatory 型を含む場合は利用側が未対応と区別できること。
- immutable 内も検査し、GREASE を未知 Mandatory と誤認しないこと。
- 単発要求の FIN・破棄・ローカル cancel 済みの遅延応答を既存の方針と整合させること。
- 未対応結果が応答待ちのまま残らず、通常の成功通知も発行されないこと。専用イベントから Request ID と未対応型を参照でき、応答タイマーが解除されること。

## 依存・関連

- 関連: [0385](0385-moqt-draft-23-unsupported-property-error-detail.md)

## 解決方法

- `src/session/core.rs` の `handle_peer_request_ok` は既に Track Properties を
  `handle_ok_for_track_status` へ渡していたため、core.rs は変更せず、応答の意味判定を行う
  `handle_ok_for_track_status` 側で未知 Mandatory Track Property を検査した。
- 判定は `TrackProperties::unknown_mandatory_types()` を使う。draft-ietf-moq-transport-23 §3.7
  (Mandatory to Understand Track Properties) の 0x4000-0x7FFF のうち、§16.8 (Properties) の
  既知型と §13 (Grease) の GREASE 値を除いた型を、§10.7 (Immutable Properties) の
  "MUST search both" に従って mutable リストと IMMUTABLE_PROPERTIES の内側の両方から
  昇順・重複なしで列挙する。GREASE の範囲衝突は `TrackProperties::unknown_mandatory_types`
  の既存の解釈をそのまま使う。
- 未知の型があれば `TrackStatusResponse::Unsupported` を記録し、
  `SessionEvent::TrackStatusUnsupported { request_id, unsupported_property_types }` を発行する。
  応答は受領済みであるため「応答待ち」には戻さず、通常の `RequestOkReceived` は発行しない。
  この判定は §8.4 / §12.1 条件 8 / 9 のスコープ・多重検証より先に行う
  (draft が両条件の優先順位を定めておらず、subscription / FETCH の経路と同じ順序に揃えるため)。
- §3.7 がリレーに求める downstream の TRACK_STATUS への TRACK_STATUS_ERROR 伝播は、
  CODEBASE.md の方針どおり追加していない。既に届いた TRACK_STATUS_OK に対して逆向きに
  REQUEST_ERROR を返すこともしない (peer は応答を送り終えており、返すと応答の二重送信になる)。
- 未対応結果でも `clear_control_message_deadline` を呼んで応答タイマーを解除する。
  `is_track_status_discardable` は `response` の有無だけを見るため、responder の FIN を受けた
  時点の `FinishRequestStream` / `RequestTerminated` と `forget_track_status` の受理条件は
  通常の TRACK_STATUS_OK と同じである。
- `handle_ok_for_track_status` は、ローカル終端済み (`rejected_request_ids` に記録済み) の
  entry への遅延応答を role 検証の後に吸収し、状態遷移とイベント発行を行わない。
  この実装が TRACK_STATUS の requester entry をローカル終端するのは、同じ Track の
  Malformed Track 検出が sibling として cancel した場合である (§6.4.2.2 (Graceful Request
  Stream Closure) により、自側の reset 後も peer が送った TRACK_STATUS_OK は正当に届く)。
  subscription / FETCH の遅延応答吸収と同じ方針に揃え、未対応通知も発行しない。
- `crates/tokio-moq/src/moqt_client.rs` の `drain_events` と `is_notable_event` に
  `TrackStatusUnsupported` を notable イベントとして追加し、アプリが未対応の型を参照できる
  ようにした。
- `skills/shiguredo-moqt/SKILL.md` の `SessionEvent` 表と `track_properties` のアクセサ一覧に
  専用イベントと `unknown_mandatory_types` を追記した。

## 完了条件の検証

- 通常の Track Properties は成功として扱い、未知 Mandatory 型を含む場合は利用側が未対応と
  区別できること: 既存の `tests/test_session/track_status.rs` の
  `track_status_completes_on_request_ok` / `track_status_ok_with_include_properties_one_keeps_track_properties`
  が通常の成功を固定する。追加した `track_status_ok_with_unknown_mandatory_property_is_unsupported`
  が、entry の `response` が `TrackStatusResponse::Unsupported` になり、`RequestOkReceived` を
  発行せず、`TrackStatusUnsupported` の `request_id` と型一覧を参照できることを検証する。
  wire 入力は `track_status_ok_with_unknown_mandatory_property_on_wire_is_unsupported` が
  `ControlMessage::decode` 経由で固定する。逆向きの REQUEST_ERROR を送らないことも前者が
  `SendOnStream` を走査して検証する。
- immutable 内も検査し、GREASE を未知 Mandatory と誤認しないこと: フィクスチャ
  `properties_with_unknown_mandatory` は mutable の 0x5000、IMMUTABLE_PROPERTIES の内側の
  0x6000、GREASE 値 0x401D、既知型 0x0E を持ち、イベントの型一覧が `[0x5000, 0x6000]` に
  なることを両テストで検証する。GREASE 値だけを含む OK は
  `track_status_ok_with_grease_property_is_success` が通常の成功 (`TrackStatusResponse::Ok`
  と `RequestOkReceived`) として扱い、`TrackStatusUnsupported` を発行しないことを検証する。
- 単発要求の FIN・破棄・ローカル cancel 済みの遅延応答を既存の方針と整合させること:
  `track_status_unsupported_allows_fin_exchange_and_forget` が responder の FIN に対する
  `FinishRequestStream` と `RequestTerminated { kind: TrackStatus }`、および
  `forget_track_status` の受理を検証する。
  `late_request_ok_for_locally_cancelled_track_status_is_absorbed` が、Malformed Track で
  ローカル終端済みの entry に遅延して届いた OK を吸収し、`Error` のまま維持して
  `TrackStatusUnsupported` を発行しないことを検証する。
- 未対応結果が応答待ちのまま残らず、通常の成功通知も発行されないこと。専用イベントから
  Request ID と未対応型を参照でき、応答タイマーが解除されること:
  `track_status_unsupported_clears_control_message_deadline` が
  `set_control_message_timeout_ms(Some(10))` の deadline を 100ms 進めても
  `CloseSession` が発行されず `Established` のままであることを検証する。応答待ちの解消は
  上記の `response` の検証が兼ねる。
- 検証コマンド: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo test --workspace`、`RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` が
  すべて成功した。
