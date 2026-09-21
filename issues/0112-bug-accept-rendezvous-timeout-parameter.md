# 定義済みパラメータ RENDEZVOUS_TIMEOUT を未知として拒否しない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-accept-rendezvous-timeout-parameter
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §9.20.7 (RENDEZVOUS TIMEOUT Parameter) は `RENDEZVOUS_TIMEOUT` (Parameter Type 0x04) を定義し、SUBSCRIBE への出現を認めている。

> The RENDEZVOUS_TIMEOUT parameter (Parameter Type 0x04) MAY appear in
> a SUBSCRIBE message.

§16.7 (Message Parameters) の Table 13 にも `0x04 RENDEZVOUS_TIMEOUT Section 9.20.7` として登録されている。一方、§9.20 (Control Message Parameters) が PROTOCOL_VIOLATION を要求するのは negotiated version で定義されていないパラメータである。

> All Message Parameters MUST be defined in the negotiated version of
> MOQT or negotiated via Setup Options.  An endpoint that receives an
> unknown Message Parameter MUST close the session with
> PROTOCOL_VIOLATION.

0x04 は draft-ietf-moq-transport-21 で定義済みであるため、これを未知として扱ってセッションを閉じるのは過剰である。本ライブラリは relay を実装しないため RENDEZVOUS_TIMEOUT の semantics を解釈しないが、解釈しないことと受信でセッションを落とすことは別である。relay を経由した subscriber が 0x04 を載せた SUBSCRIBE を送ると、本ライブラリを使う publisher がセッションを閉じてしまう interop 上の問題を解消する。

## 現状

拒否ゲートは 2 つある。

- `src/message_parameter.rs` の `value_encoding` に 0x04 の分岐が無く `MessageError::ProtocolViolation("unknown message parameter type")` を返す。closed issue 0086 で `PARAM_RENDEZVOUS_TIMEOUT` と `MessageParameters::rendezvous_timeout` を削除した結果である。
- `src/message.rs` の `SUBSCRIBE_ALLOWED_PARAMS` に 0x04 が無いため、`Subscribe::encode_message_body` と `Subscribe::decode_message_body` の `MessageParameters::validate_scope` でも `ProtocolViolation` になる。

`tests/test_message.rs` の `subscribe_with_unhandled_type_0x04_is_rejected` が encode 側の拒否を固定している。`CHANGES.md` の `## develop` の RENDEZVOUS_TIMEOUT 削除エントリと、`docs/IMPLEMENTATION.md` の「その他、relay 専用の以下のパラメータも実装しない」にも 0x04 が残っている。

`Session::recv_request` は decode 済みの `ControlMessage` を受け取るため、decode 段階の拒否はセッションを維持したまま回復できない。I/O 層はデコードエラーをセッション終了として扱う (`MessageError::InvalidMessageType` の doc と同じ扱い)。

## 設計方針

- `value_encoding` に 0x04 (vi64 のミリ秒) を追加し、`SUBSCRIBE_ALLOWED_PARAMS` にも 0x04 を加えて SUBSCRIBE で受理する。
- 受信した 0x04 の扱いを決めて明記する。relay 専用の購読保持 semantics (publisher の出現待ち、TIMEOUT 応答) は実装対象外とし、購読パラメータとして保持するのみで解釈しない。アプリへ値を渡すかどうかも併せて決める。
- 送信側 (`src/message.rs` の `Subscribe`) で 0x04 を指定できるようにするかは、受信側の扱いと対称に決める。受理のみで送信 API を持たない選択もありうる。
- 未知パラメータを PROTOCOL_VIOLATION にする §9.20 の処理はそのまま残す。`tests/test_message.rs` の `subscribe_with_unhandled_type_0x04_is_rejected` は、Table 13 に無い未定義の型を拒否することを固定するテストへ置き換える。
- 0086 で削除した `Subscription::subscriber_rendezvous_timeout_ms` を戻すかは受信側の扱いの決定に従う。値を library 内部で使わないなら戻さない。
- `CHANGES.md` と `docs/IMPLEMENTATION.md` の記述を実装に合わせて更新する。

## 完了条件

- 0x04 を含む SUBSCRIBE が `ControlMessage::decode` で受理されることがテストで固定されていること。
- `Session::recv_request` に 0x04 付き SUBSCRIBE を渡してもセッションが `Closing` に遷移しないことがテストで固定されていること。受理後の 0x04 の扱い (保持のみ・無視・アプリへの通知のいずれか) も同じテストで固定する。
- Table 13 に無いパラメータ型が引き続き `ProtocolViolation` で拒否されることがテストで固定されていること。
- `docs/IMPLEMENTATION.md` と `CHANGES.md` の記述が実装と一致していること。
- `cargo test --workspace` が通ること。
