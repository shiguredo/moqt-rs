# FETCH_OK を REQUEST_OK に統合して応答範囲を導出する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-fetch-ok-request-ok
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.2、§3.2.2、§9.3、§9.11、§9.19.17](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` は Type `0x18` の `FetchOk` に `end_of_track` / `end_location` を持たせる。
`src/session/fetch.rs` の `send_fetch_ok` / `handle_peer_fetch_ok` と `SessionEvent::FetchOkReceived`、`crates/tokio-moq/src/moqt_client.rs` / `crates/moq-sub/src/pipeline.rs` もこの専用応答を前提にしている。

## 設計方針

FETCH 初回成功を Type `0x07` の REQUEST_OK として扱い、FETCH の REQUEST_UPDATE_OK とは状態で区別する。
EXPIRES / LARGEST_OBJECT と Track Properties を処理し、旧 End Location / End Of Track のフィールドを廃止する。
LARGEST_OBJECT が要求範囲内なら応答に必須とし、応答の終点をそこへ切り詰める。
それ以外は要求 End Location を用いる。
要求省略時の範囲も含め、FETCH_OK の送信時点で終点を判断できるようにする。
Track 終端はデータ側の新しい指示で受け取る。

## 完了条件

- 初回 FETCH_OK と FETCH の REQUEST_UPDATE_OK を正しく dispatch し、Track Properties を初回応答だけで受理すること。
- LARGEST_OBJECT の範囲内・範囲外・省略と、応答前のデータ到着・空ストリーム・降順の終端範囲を検証すること。
- 旧 Type `0x18` を現行 FETCH_OK として受理せず、利用側イベントと送信 API が新しい範囲モデルに一致すること。

## 依存・関連

- 先行: [0398](0398-moqt-draft-23-fetch-group-track-end.md)
