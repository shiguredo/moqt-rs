# C4M の SETUP 統合と SUBSCRIBE_TRACKS action に対応する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-c4m-draft-02-action-codes
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §2.1 Table 1、§4.2](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `MoqtAction` は ClientSetup=0 / ServerSetup=1 を別 action とし、ALL は 9 要素で、SUBSCRIBE_TRACKS=9 がない。
`authorization_context` は旧 DPoP の文字列 action も返す。

## 設計方針

draft 02 の SETUP=1 と SUBSCRIBE_TRACKS=9 に合わせ、enum、数値の変換、表示と scope の評価を更新する。
`ClientSetup` / `ServerSetup` を `Setup` に統合し、`SubscribeTracks` を追加する。`ALL` は 1〜9 の 9 要素とし、`key` / `from_key` / `name` と利用箇所を揃える。
旧 ClientSetup=0 は未知の整数として scope 内に保持できるが、`from_key(0)` は `None` とし、既知 action を認可する alias にしない。他の未知整数も同じ扱いとする。
旧 JWT 用の文字列 `authorization_context` は互換 API として残し、戻り値を `Option<&'static str>` にする。`Setup` は `Some("SETUP")`、旧形式に定義のない `SubscribeTracks` は `None` とする。`matches_authorization_context` と旧 JWT 側の呼び出し箇所は未対応 action を明示的に拒否し、独自の文字列 action を追加しない。
CWT の数値 actx は同じ action 定義を使うが、その codec への組み込みと照合は 0416 / 0418 で行う。本 issue の完了に CWT proof の先行実装を要求しない。
SUBSCRIBE_TRACKS の認可表現を追加しても、namespace 機能の実装は行わない。

## 完了条件

- SETUP=1 と SUBSCRIBE_TRACKS=9 の既知バイト列・認可を検証すること。
- 未知 action と旧 0 を既知 action の許可として扱わず、要求 action を含まない scope は拒否すること。
- 旧 JWT 用の文字列 API が `Setup` を扱い、`SubscribeTracks` を未対応として拒否すること。CWT proof と scope の数値 action の一致は 0416 / 0418 で検証すること。

## 依存・関連

- 関連: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
