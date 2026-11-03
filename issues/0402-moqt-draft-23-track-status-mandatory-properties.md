# TRACK_STATUS_OK の未知 Mandatory Track Property を検査する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-track-status-mandatory-properties
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.7、§9.3、§9.12](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/core.rs` の `handle_peer_request_ok` は Track Properties を TRACK_STATUS_OK で受理するが、`handle_ok_for_track_status` へはパラメータだけを渡す。
`src/session/track_status.rs` の同関数にも `has_unknown_mandatory` に相当する検証がなく、未対応の Track を通常の成功として処理できる。

## 設計方針

TRACK_STATUS_OK を処理する前に未知 Mandatory Track Property を確認し、当該 Track を利用可能な成功として処理しない。
非リレー endpoint として結果・未対応理由を利用側へ通知できる状態と API を定める。
既に届いた OK に対して逆向きに REQUEST_ERROR を返さない。
draft が記載する downstream へのエラー伝播はリレー機能のため追加しない。

## 完了条件

- 通常の Track Properties は成功として扱い、未知 Mandatory 型を含む場合は利用側が未対応と区別できること。
- immutable 内も検査し、GREASE を未知 Mandatory と誤認しないこと。
- 単発要求の FIN・破棄・ローカル cancel 済みの遅延応答を既存の方針と整合させること。

## 依存・関連

- 関連: [0385](0385-moqt-draft-23-unsupported-property-error-detail.md)
