# DATAGRAM の DEFAULT_PRIORITY 継承元を購読を確立したメッセージの値にする

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-datagram-default-priority-inheritance
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §11.2.1 (Object Datagram) と §11.3.1 (Subgroup Header) は、
DEFAULT_PRIORITY bit が立っている Datagram / Subgroup が「購読を確立した制御メッセージで
指定された Publisher Priority」を継承すると定めている。この値は §10.4 (DEFAULT PUBLISHER PRIORITY)
の Track Property であり、宣言が無ければ 128 になる。現状の datagram の解決は直近の
SUBGROUP_HEADER の解決値を優先するため、明示 Publisher Priority 付きの Subgroup の後に届く
DEFAULT_PRIORITY の Datagram が誤った優先度で評価される。

## 現状

- `src/session/data.rs` の `recv_subgroup_header` は解決済みの Publisher Priority を
  `Subscription::publisher_priority` に上書きする。
- `src/session/types.rs` の `Subscription::effective_publisher_priority` は `publisher_priority` →
  `default_publisher_priority` → 128 の順に解決する。`publisher_priority` の読み手はこの関数だけで、
  書き手は `recv_subgroup_header` だけである。
- `effective_publisher_priority` の呼び出し元は datagram の 3 経路だけである。
  - `src/session/data.rs` の `send_object_datagram` のローカルフィルタ検査
  - `src/session/data.rs` の `recv_object_datagram` の候補購読のフィルタ評価
  - `src/session/data.rs` の重複 Object の Forwarding Preference / Subgroup ID / Priority 一貫性検証
- `send_object_datagram` は Publisher Priority を引数に取らないため、送信する datagram は常に
  DEFAULT_PRIORITY bit が立った状態になる。送信側でも同じ誤りが起きる。
- Subgroup 受信時の `Subscription::resolve_header_publisher_priority` の使い方自体は正しい。
  §12.1 (Malformed Tracks) の条件 1 の priority 一致検証とフィルタ評価は
  `IncomingDataStream::Subgroup` が header 時点で保持する解決値を使っており、
  `Subscription::publisher_priority` に依存していない。

根拠 (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram)):

> The *DEFAULT_PRIORITY* bit (0x08) indicates when the Priority field is present.
> When set to 1, the Priority field is omitted and this Object inherits the Publisher Priority
> specified in the control message that established the subscription.

根拠 (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header)):

> The *DEFAULT_PRIORITY* bit (0x20) indicates when the Priority field is present.
> When set to 1, the Priority field is omitted and this Subgroup inherits the Publisher Priority
> specified in the control message that established the subscription.

根拠 (draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY)):

> Subgroups and Datagrams for this subscription inherit this priority, unless they specifically
> override it.

> If omitted, the Default Publisher Priority is 128.

再現手順:

1. DEFAULT_PUBLISHER_PRIORITY を 42 として購読を確立する
2. Publisher Priority を明示した SUBGROUP_HEADER を受信し、`Subscription::publisher_priority` を
   7 にする
3. DEFAULT_PRIORITY bit が立った datagram を受信する
4. 期待は 42 での評価だが、`effective_publisher_priority` が直近値 7 を返すため 7 で評価される

## 設計方針

- datagram の DEFAULT_PRIORITY 解決を `Subscription::resolve_header_publisher_priority(None)`
  (Track Property の DEFAULT_PUBLISHER_PRIORITY → 128) に統一し、datagram の 3 経路を置き換える。
- 置き換え後に `Subscription::publisher_priority` と `Subscription::effective_publisher_priority`
  の読み手が無くなる。この 2 つは誤った継承元を参照させるために存在していたものなので、
  同種の誤用を再発させないよう削除し、`recv_subgroup_header` の上書きも削除する。
  これは本不具合の原因そのものの除去であり、無関係なリファクタリングは含めない。
- Subgroup 単位の検証とフィルタ評価は、`IncomingDataStream::Subgroup` が保持する解決値と
  `resolve_header_publisher_priority` を引き続き使う。
- `tests/test_session/default_publisher_properties.rs` の `publisher_priority` /
  `effective_publisher_priority` を参照するアサーションを、公開 API の送受信結果で検証する形に直す。
- 明示 Publisher Priority 付き Subgroup の後に DEFAULT_PRIORITY の datagram を流し、
  Track Property の値で評価される回帰テストを送信側と受信側に追加する。

## 完了条件

- DEFAULT_PRIORITY bit が立った datagram が、直近 SUBGROUP_HEADER の Publisher Priority ではなく
  購読を確立したメッセージの DEFAULT_PUBLISHER_PRIORITY (無ければ 128) で評価されること
- 送信側と受信側で同じ解決結果になること
- `Subscription::publisher_priority` と `Subscription::effective_publisher_priority` が
  削除されていること
- 回帰テストが `tests/test_session/` に追加され、`cargo test --workspace` が通ること
- `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all -- --check` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること
