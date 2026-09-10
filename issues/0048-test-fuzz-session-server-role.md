# fuzz_session に server role と送信 API を追加する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/test-fuzz-session-server-role
- Polished: 2026-09-10

## 目的

`Session` の server role と送信側の状態遷移を fuzzing の対象に含め、受信系だけでは検出できない不整合を拾う。

## 現状

- `fuzz/fuzz_targets/fuzz_session.rs` は `Session::new_client` のみを使い、`Op` は `recv_*` / `tick` / `close` のみで送信 API を呼ばない。
- 送信 API（`Session::send_subscribe` / `Session::send_fetch` 等）は先頭で `require_established()` を呼ぶ。`new_client` / `new_server` 直後の状態は `LocalSetupSent` で、`Established` になるのは有効な peer SETUP を `recv_control` で受けたときだけ。
- 現行の `Op::RecvControl` は任意バイト列を `ControlMessage::decode` に通すだけで、SETUP 成立は偶然に依存する。SETUP / GOAWAY 以外を渡すと `PROTOCOL_VIOLATION` で `Closing` に落ちる。このため送信 `Op` を追加しても大半は `require_established` エラーや Closing で no-op になり、送信側の状態遷移を fuzz できない。
- role は target 起動時に 1 回だけ決まり、`Op` はステップ単位の列挙である。現行は client 固定。
- `pbt/tests/prop_session/common.rs` の `establish_pair` は `new_client` / `new_server` の両方を生成して SETUP を相互注入しており、両 role と送信 API を扱う。完全な欠落ではない。

## 設計方針

- 入力を role 選択（`bool`）と操作列（`Vec<Op>`）に分ける。`bool` から `new_client` / `new_server` を選ぶ。
- 操作列の前に `Established` へ遷移させる段を置く。`pbt/tests/prop_session/common.rs` の `establish_pair` 相当の手順（両端で SETUP を相互注入）を fuzz にも持ち込み、pbt のヘルパを共有できるか検討する。ハンドシェイク自体を入力で揺らす場合は、SETUP 専用の `Op` を設けて現行 `Op::RecvControl` の役割と書き分ける。
- 送信 API の `Op` を追加する。引数は既存の decode 方式に合わせ、生バイトから `ControlMessage::decode` して得た `Subscribe` / `Publish` / `Fetch` を `send_subscribe` / `send_publish` / `send_fetch` に流す。`MessageParameters` は `MessageParameters::new()` 固定、または同様に decode 経由とする。対象 API に応答系（`send_subscribe_ok` /
  `send_request_ok` / `send_request_error` 等）を含めるかは実装時に確定し、含める API を列挙する。
- 送信で発行された request_id を保持し、後続の受信 `Op` に相関させるかを決める。相関させる場合は受信 `Op` の request_id を入力から与えず保持値を使い、相関させない場合は「送信側はイベント排出までを検証対象にする」と明記する。
- クラッシュが検出された場合は、本 issue は target 追加の完了とし、原因調査は別 issue とする。

## 完了条件

- `fuzz_session` が client / server 両 role を入力で選べること（`bool` の両値で target が動作すること）
- 操作列の前に `Established` へ遷移する手順があり、送信 API が `require_established` で落ちずに呼ばれること
- 送信 API が操作列に含まれ、送信側の状態遷移とイベント排出が fuzz で実行されること
- `cargo check --manifest-path fuzz/Cargo.toml` が通ること
- `cargo fuzz run fuzz_session -- -max_total_time=<秒>` を実行し、指定時間起動し続けること（クラッシュの有無は記録し、クラッシュした場合は原因調査を別 issue とする）
