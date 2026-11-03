# REQUEST_ERROR に構造化された Error Details を追加する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-request-error-details
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.4.1、§9.4.2、§9.4.2.1、§16.11](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `RequestError` は `redirect: Option<Redirect>` を持ち、REDIRECT の場合だけ独自の構造を本文末尾へ直結する。
他のエラーコードの末尾データは長さ不一致として拒否する。
`src/session/core.rs` の `validate_peer_redirect` とエラー送信 API もこの表現を使う。

## 設計方針

Error Reason の後を Delta Type 付き KVP の Error Details 列に変更する。
REDIRECT は Type `0x01` の length-prefixed Error Detail とし、REDIRECT error では必須にする。
未知 Type と、既知だが当該 Error Code に適用しない Detail を無視する。
未知 Type の重複は許可し、既知 Type の多重可否は定義に従う。
namespace 用 REDIRECT の空 Track Name と、server の非空 Connect URI 拒否を新形式でも保つ。
公開送信 API とイベントに Detail を渡す手段を用意する。

## 完了条件

- REDIRECT の golden バイト列と、必須 Detail 欠落時の MALFORMED_MESSAGE を検証すること。
- 未知 Detail・重複未知 Detail・適用外の既知 Detail が不当にセッションを閉じないこと。
- Detail の KVP 境界・全体消費量と、REDIRECT の role / field 検証を確認すること。
