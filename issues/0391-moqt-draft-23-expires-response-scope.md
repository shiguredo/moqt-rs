# TRACK_STATUS_OK と FETCH_OK の EXPIRES を扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-expires-response-scope
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.3、§9.19.16](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `TRACK_STATUS_OK_ALLOWED_PARAMS` は LARGEST_OBJECT だけであり、`src/session/core.rs` が EXPIRES を拒否する。
`src/session/track_status.rs` の応答 state も EXPIRES を保持しない。
FETCH_OK は専用メッセージでパラメータを許していない。

## 設計方針

EXPIRES をすべての OK 応答で許可する。
FETCH_OK の REQUEST_OK 統合と整合させる。
TRACK_STATUS_OK の値は仮に SUBSCRIBE_OK を返すなら提示した値であり、終了済みの TRACK_STATUS 要求を再び生かしたり期限タイマーを起動したりしない。
受信イベントまたは応答参照 API で値を利用できるようにする。

## 完了条件

- TRACK_STATUS_OK / FETCH_OK の EXPIRES が codec と直接 Session API の双方で受理されること。
- TRACK_STATUS_OK の EXPIRES を仮の購読期限として参照でき、単発要求の FIN・破棄条件に影響しないこと。
- 既存の PUBLISH_OK / REQUEST_UPDATE_OK / SUBSCRIBE_OK の許可条件を維持すること。

## 依存・関連

- 関連: [0382](0382-moqt-draft-23-fetch-ok-request-ok.md)
