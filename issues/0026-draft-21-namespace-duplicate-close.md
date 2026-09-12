# NAMESPACE 重複受信で draft に無い PROTOCOL_VIOLATION を返さない

- Created: 2026-09-10
- Completed: 2026-09-13
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
- `TRACK_NAMESPACE_PREFIX` を更新した後に同一 suffix 文字列が届くケースも対象に含める。0017 の実装時に `active_suffixes` の再キー付けは行われなかったため、本 issue で full namespace 単位の照合と投影の作り直しを実装する (coalescing した responder の確定待ち prefix も照合候補に含める)。

## 完了条件

- 同一 suffix の NAMESPACE を 2 回受信しても `CloseSession` が発生せず、`recv_stream_message` が `Err` を返さず、セッション state が `Established` のままであること
- 2 回目の受信で `SessionEvent::NamespaceReceived` が再発行されず、`active_suffixes` に当該 suffix が 1 件だけ残ること
- `NAMESPACE_DONE` 先行の `PROTOCOL_VIOLATION` と first frame 制約など、重複以外の既存順序検証が維持されていること
- 重複 NAMESPACE の回帰テストが `tests/test_session/namespace/subscribe_namespace.rs` に追加され、`cargo test --workspace` が通ること

## 解決方法

同一 suffix の NAMESPACE 再受信でセッションを閉じないようにし、prefix 更新を跨いだ重複判定と NAMESPACE_DONE 照合を full namespace 単位で行うようにした。

- `src/session/core.rs`: `NamespaceState` に `active_full_namespaces: HashMap<u64, HashSet<Vec<Vec<u8>>>>` を追加した。受信時点の prefix で解決した full namespace のフィールド列で active 集合を管理する。
- `src/session/namespace/subscribe_namespace.rs`: `handle_peer_namespace` は候補 prefix (現在 + 確定待ち) で解決した
  full namespace が既に active なら無視し、`NamespaceReceived` を再発行しない。`handle_peer_namespace_done` は
  同じ候補 prefix で照合し、一致しなければ §9.15 の PROTOCOL_VIOLATION、一致すれば削除して投影を作り直す。
  `handle_ok_for_namespace_subscription` の prefix 適用時は `active_suffixes` を現在 prefix 配下の投影として
  作り直す (旧基準の full namespace は coalescing の DONE 照合用に保持)。REQUEST_ERROR / bidi 終端 / forget では
  full namespace 集合も破棄する。
- `src/session/types.rs`: `active_suffixes` を「現在 prefix 配下の suffix 投影」と定義し、`NamespaceReceived` / `NamespaceDoneReceived` の doc に full namespace 単位の照合と coalescing の扱いを追記した。
- `tests/test_session/namespace/subscribe_namespace.rs` に、重複無視 (DONE 後の再告知含む)、prefix 更新後の新基準同一 suffix、prefix 変更なし更新での保持、coalescing の revert (両 REQUEST_OK 後 / REQUEST_OK 間) 、prefix 短縮時の投影、旧 prefix 基準 DONE の違反を検証する回帰テストを追加した。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。
