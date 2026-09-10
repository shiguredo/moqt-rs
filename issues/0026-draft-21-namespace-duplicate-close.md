# NAMESPACE 重複受信で draft に無い PROTOCOL_VIOLATION を返さない

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-namespace-duplicate-close

## 目的

draft-ietf-moq-transport-21 が要求しない条件でセッションを閉じないようにする。SUBSCRIBE_NAMESPACE 応答で同一 suffix の NAMESPACE を再受信しても相互運用を損なわないようにする。

## 現状

`src/session/namespace/subscribe_namespace.rs` は、同一 suffix の `NAMESPACE` を再受信すると `PROTOCOL_VIOLATION` でセッションを閉じる。コメント自身が「draft が MUST で禁止していない」と認めている。

根拠: draft-ietf-moq-transport-21 §9.15 は、`NAMESPACE_DONE` が対応する `NAMESPACE` より先に来た場合の `PROTOCOL_VIOLATION` のみ規定し、`NAMESPACE` の重複を違反としていない。

## 設計方針

同一 suffix の `NAMESPACE` 再受信は「無視 (イベントを再発行しない)」に留める。仕様外の close をやめる。

## 完了条件

- 同一 suffix の NAMESPACE 再受信でセッションが閉じないこと
- 既存の NAMESPACE / NAMESPACE_DONE の順序検証は維持されること
- 重複 NAMESPACE のテストが `tests/test_session/namespace/subscribe_namespace.rs` に追加されていること
