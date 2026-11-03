# TRACK_STATUS_OK の未知 Mandatory Track Property を検査する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-track-status-mandatory-properties
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.7、§9.3、§9.12](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/core.rs` の `handle_peer_request_ok` は Track Properties を TRACK_STATUS_OK で受理するが、`handle_ok_for_track_status` へはパラメータだけを渡す。
`src/session/track_status.rs` の同関数にも `has_unknown_mandatory` に相当する検証がなく、未対応の Track を通常の成功として処理できる。

## 設計方針

TRACK_STATUS_OK を処理する前に未知 Mandatory Track Property を確認し、当該 Track を利用可能な成功として処理しない。
`handle_peer_request_ok` から `handle_ok_for_track_status` へ Track Properties も渡す。未知 Mandatory 型があれば、`TrackStatusResponse::Unsupported` を追加して応答受領済みのローカル未対応結果を記録する。通常の `RequestOkReceived` は発行せず、専用の `SessionEvent::TrackStatusUnsupported` で Request ID と未対応 Property Type の一覧を通知する。
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
