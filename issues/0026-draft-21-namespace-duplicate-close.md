# NAMESPACE 重複受信で draft に無い PROTOCOL_VIOLATION を返さない

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-namespace-duplicate-close
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 が要求しない条件でセッションを閉じないようにする。SUBSCRIBE_NAMESPACE 応答で同一 suffix の NAMESPACE を再受信しても、セッションを閉じずエラーも返さず相互運用を損なわないようにする。

## 現状

`src/session/namespace/subscribe_namespace.rs` の `handle_peer_namespace` は、`active_suffixes.insert()` が false になったとき `SESSION_PROTOCOL_VIOLATION` を生成し、`self.fail()` と `Err` の両方を返す。`fail()` は `SessionState::Closing` への遷移と `SessionEvent::CloseSession` を発生させ、`Err` は
`recv_stream_message` の呼び出し元へ伝播する。コメント自身が「同一 suffix の NAMESPACE 重複は draft が MUST で禁止していない」と認めている。

根拠 (draft-ietf-moq-transport-21 §9.15 / §9.16 / §9.17):

- §9.15 は `NAMESPACE_DONE` が対応する `NAMESPACE` より先に来た場合を含め複数の `PROTOCOL_VIOLATION` MUST を持つが、NAMESPACE の重複受信を違反とする規定は §9.15 / §9.16 / §9.17 のいずれにもない。
- §9.15: "The publisher MUST NOT send NAMESPACE_DONE for a namespace suffix before the corresponding NAMESPACE. If a subscriber receives a NAMESPACE_DONE before the corresponding NAMESPACE, it MUST close the session with a 'PROTOCOL_VIOLATION'."

## 設計方針

- 同一 suffix の `NAMESPACE` 再受信は「無視」とする。`self.fail()` を呼ばず、`Err` も返さず、`NamespaceReceived` を再発行しない。`active_suffixes` の一意性は維持される。
- 既存の順序検証はすべて維持する。`NAMESPACE_DONE` 先行の `PROTOCOL_VIOLATION`、first frame 制約、Track Namespace Prefix の 32 fields 上限など、重複以外の MUST は変更しない。
- 対象外: `TRACK_NAMESPACE_PREFIX` を更新した後に同一 suffix 文字列が届くケース (suffix は新 prefix 相対になるため、旧 prefix で受理した suffix と同一とは限らない)。prefix のローカル反映は open issue 0017 が扱っており、`active_suffixes` を prefix 更新時に破棄または再キー付けするかの判断は 0017 の実装時に併せて行う。本 issue は prefix が変わらない間の同一 suffix 重複を対象とする。

## 完了条件

- 同一 suffix の NAMESPACE を 2 回受信しても `CloseSession` が発生せず、`recv_stream_message` が `Err` を返さず、セッション state が `Established` のままであること
- 2 回目の受信で `SessionEvent::NamespaceReceived` が再発行されず、`active_suffixes` に当該 suffix が 1 件だけ残ること
- `NAMESPACE_DONE` 先行の `PROTOCOL_VIOLATION` と first frame 制約など、重複以外の既存順序検証が維持されていること
- 重複 NAMESPACE の回帰テストが `tests/test_session/namespace/subscribe_namespace.rs` に追加され、`cargo test --workspace` が通ること
