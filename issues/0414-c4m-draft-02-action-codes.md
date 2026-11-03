# C4M の SETUP 統合と SUBSCRIBE_TRACKS action に対応する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-c4m-draft-02-action-codes
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-c4m-02 §2.1 Table 1、§4.2](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `MoqtAction` は ClientSetup=0 / ServerSetup=1 を別 action とし、ALL は 9 要素で、SUBSCRIBE_TRACKS=9 がない。
`authorization_context` は旧 DPoP の文字列 action も返す。

## 設計方針

draft 02 の SETUP=1 と SUBSCRIBE_TRACKS=9 に合わせ、enum、数値の変換、表示と scope の評価を更新する。
CWT の actx では同じ数値 action を使い、旧 ClientSetup=0 を SETUP の無条件 alias としない。
SUBSCRIBE_TRACKS の認可表現を追加しても、namespace 機能の実装は行わない。

## 完了条件

- SETUP=1 と SUBSCRIBE_TRACKS=9 の既知バイト列・認可を検証すること。
- 未知 action と旧値の扱いが明示され、action の違う proof / scope を受理しないこと。
- DPoP の新しい数値 actx と scope の action が一致すること。

## 依存・関連

- 関連: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
