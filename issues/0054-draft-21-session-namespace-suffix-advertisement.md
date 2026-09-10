# 空 prefix の NAMESPACE / PUBLISH_SKIPPED で予約名前空間を広告できないようにする

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-session-namespace-suffix-advertisement
- Polished: {YYYY-MM-DD}

## 目的

SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の応答で Application が予約名前空間を広告できないようにする。prefix が空の場合、suffix の先頭フィールドが `.session` / `.` だと full namespace が予約名前空間になり、§6.5 の Application MUST NOT と §2.4.2 の MUST NOT に違反する。

## 現状

- `send_publish` と `send_publish_namespace` は `.session` (および `.`) をローカル拒否する。
- `send_namespace` / `send_namespace_done` / `send_publish_skipped` は suffix をそのまま書き出す。full namespace は購読の prefix と suffix の連結で決まる。
- SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の prefix は 0 フィールドも合法 (draft §9.15 / §9.18: "between 0 and 32 Track Namespace Fields")。prefix が空の購読では suffix が full namespace の先頭になる。
- 再現手順: 空 prefix の SUBSCRIBE_NAMESPACE を確立し、購読側が suffix `[".session"]` で `send_namespace` を呼ぶと、full namespace が `.session` の NAMESPACE が wire に載る。

根拠 (draft-ietf-moq-transport-21 §6.5):

> "The Application MUST NOT publish tracks or namespaces whose first field is .session."

根拠 (draft-ietf-moq-transport-21 §2.4.2):

> "A Track Namespace whose first field is exactly . [...] is reserved and MUST NOT be used for any purpose"

## 設計方針

- `NamespaceSubscription::prefix` / `TrackSubscription::prefix` が空のとき、suffix の先頭フィールドが `.session` / `.` の送信を `SESSION_PROTOCOL_VIOLATION` で拒否する。`send_namespace` / `send_namespace_done` / `send_publish_skipped` の 3 API が対象。
- prefix が非空なら full namespace の先頭フィールドは prefix の先頭であり、初回購読の時点で予約名前空間は拒否されているため追加検証しない。
- 受信側 (`handle_peer_namespace` / `handle_peer_publish_skipped`) の対称検証は本 issue の対象外とする。受信した NAMESPACE の予約名前空間をどう扱うかは §6.5 に request 以外の規定がなく、必要になった時点で別途判断する。
- 送信 API からの広告経路の回帰テストを追加する。

## 完了条件

- 空 prefix の購読で suffix の先頭が `.session` / `.` の `send_namespace` / `send_namespace_done` / `send_publish_skipped` が `SESSION_PROTOCOL_VIOLATION` を返すこと
- 非空 prefix の購読では同じ suffix が従来どおり送信できること (full namespace が予約名前空間にならないため)
- 回帰テストが追加され、`cargo test --workspace` が通ること
