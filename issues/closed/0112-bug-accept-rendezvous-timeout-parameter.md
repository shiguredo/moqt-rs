# 定義済みパラメータ RENDEZVOUS_TIMEOUT を未知として拒否しない

- Created: 2026-09-21
- Completed: 2026-09-22
- Branch: feature/fix-accept-rendezvous-timeout-parameter
- Polished: 2026-09-21

## 目的

draft-ietf-moq-transport-21 §9.20.7 (RENDEZVOUS TIMEOUT Parameter) は `RENDEZVOUS_TIMEOUT` (Parameter Type 0x04) を定義し、SUBSCRIBE への出現を認めている。

> The RENDEZVOUS_TIMEOUT parameter (Parameter Type 0x04) MAY appear in
> a SUBSCRIBE message.

§16.7 (Message Parameters) の Table 13 にも `0x04 RENDEZVOUS_TIMEOUT Section 9.20.7` として登録されている。一方、§9.20 (Control Message Parameters) が PROTOCOL_VIOLATION を要求するのは negotiated version で定義されていないパラメータである。

> All Message Parameters MUST be defined in the negotiated version of
> MOQT or negotiated via Setup Options.  An endpoint that receives an
> unknown Message Parameter MUST close the session with
> PROTOCOL_VIOLATION.

0x04 は draft-ietf-moq-transport-21 で定義済みであるため、これを未知として扱ってセッションを閉じるのは過剰である。本ライブラリは relay を実装しないため RENDEZVOUS_TIMEOUT の semantics を解釈しないが、解釈しないことと受信でセッションを落とすことは別である。§9.20.1 のとおり Message Parameters は peer 向けで relay は転送しないため、0x04 を合法に載せた SUBSCRIBE を送る peer が存在しうる。それを未知パラメータとして `PROTOCOL_VIOLATION` で閉じるのは定義済みパラメータの扱いとして誤っている。

## 現状

拒否ゲートは 2 つある。

- `src/message_parameter.rs` の `value_encoding` に 0x04 の分岐が無く `MessageError::ProtocolViolation("unknown message parameter type")` を返す。closed issue 0086 で `PARAM_RENDEZVOUS_TIMEOUT` と `MessageParameters::rendezvous_timeout` を削除した結果である。
- `src/message.rs` の `SUBSCRIBE_ALLOWED_PARAMS` に 0x04 が無い。encode は `Subscribe::encode_message_body` の `validate_scope` が先に `ProtocolViolation` を返し、decode は `MessageParameters::decode` から呼ばれる `value_encoding` が先に `ProtocolViolation` を返すため `validate_scope` には到達しない

`tests/test_message.rs` の `subscribe_with_unhandled_type_0x04_is_rejected` が encode 側の拒否を固定している。`CHANGES.md` の `## develop` の RENDEZVOUS_TIMEOUT 削除エントリと、`docs/IMPLEMENTATION.md` の「その他、relay 専用の以下のパラメータも実装しない」にも 0x04 が残っている。

`Session::recv_request` は decode 済みの `ControlMessage` を受け取るため、decode 段階の拒否はセッションを維持したまま回復できない。I/O 層はデコードエラーをセッション終了として扱う (`MessageError::InvalidMessageType` の doc と同じ扱い)。

## 設計方針

- `value_encoding` に 0x04 を `ValueEncoding::VarInt` として追加し、`PARAM_RENDEZVOUS_TIMEOUT` 定数 (0x04) を戻す。§9.20.7 に値エンコーディングの明文は無いが、§9.20 の parameter value は varint で、同型の `FILL_TIMEOUT` (0x0A) も `ValueEncoding::VarInt` である。0086 の削除前実装も `VarInt` だった
- 専用のアクセサ `MessageParameters::rendezvous_timeout()` は戻さない。library は 0x04 を解釈しないため、生の `MessageParameters` を読めば足りる
- `SUBSCRIBE_ALLOWED_PARAMS` に 0x04 を加える。同定数は `Subscribe::encode_message_body` / `Subscribe::decode_message_body` / `Session::send_subscribe` で共有されるため、受信と送信の両方で受理される。0x04 専用の送信 API は追加しない
- `Session::handle_peer_subscribe` は 0x04 を解釈せず、`Subscription` にも保持しない。0086 で削除した `Subscription::subscriber_rendezvous_timeout_ms` は戻さない。値は decode 済みの `ControlMessage::Subscribe` の `parameters` に残るため、必要ならアプリが `MessageParameters` から読める
- 未知パラメータを PROTOCOL_VIOLATION にする §9.20 の処理はそのまま残す。`tests/test_message.rs` の `subscribe_with_unhandled_type_0x04_is_rejected` は、0x04 が encode / decode の両方で受理されることを固定するテストへ置き換え、Table 13 に無い未定義の型を拒否することを固定するテストを別に残す
- `skills/shiguredo-moqt/SKILL.md` のパラメータ型定数表に `PARAM_RENDEZVOUS_TIMEOUT` (`0x04`) の行を戻す (0086 で削除されている)
- `CHANGES.md` と `docs/IMPLEMENTATION.md` の記述を実装に合わせて更新する

## 完了条件

- `ControlMessage::decode` に 0x04 を含む SUBSCRIBE のバイト列を渡すと受理される (decode 経路で `value_encoding` と `validate_scope` の両方を通る) ことがテストで固定されていること
- 0x04 を含む SUBSCRIBE の encode が成功することがテストで固定されていること
- decode で得た 0x04 付き SUBSCRIBE を `Session::recv_request` に渡してもセッションが `Closing` に遷移しないことがテストで固定されていること
- 受理した 0x04 が `Subscription` に保持されない (0086 で削除した `subscriber_rendezvous_timeout_ms` が戻らない) ことがテストで固定されていること
- Table 13 に無いパラメータ型が decode と encode の両方で `ProtocolViolation` になることがテストで固定されていること
- `docs/IMPLEMENTATION.md` と `CHANGES.md` と `skills/shiguredo-moqt/SKILL.md` の記述が実装と一致していること
- `cargo test --workspace` が通ること

## 解決方法

draft-ietf-moq-transport-21 §9.20.7 (RENDEZVOUS TIMEOUT Parameter) と §16.7 (Message Parameters) の Table 13 に定義済みの `RENDEZVOUS_TIMEOUT` (Parameter Type 0x04) を、SUBSCRIBE の受信・送信の両方で受理するようにした。

1. `src/message_parameter.rs` に `PARAM_RENDEZVOUS_TIMEOUT` (0x04) を戻した。値エンコーディングは `ValueEncoding::VarInt` (§9.20.6 の `FILL_TIMEOUT` と同型)
2. `src/message_parameter.rs` の `value_encoding` に 0x04 の分岐を戻した。これにより decode 経路が `ProtocolViolation` を返さなくなる
3. `src/message.rs` の `SUBSCRIBE_ALLOWED_PARAMS` に 0x04 を加えた。同定数は `Subscribe::encode_message_body` / `Subscribe::decode_message_body` / `Session::send_subscribe` で共有されるため、受信と送信の両方で受理される
4. `MessageParameters::rendezvous_timeout` は戻さない。本ライブラリは relay を実装せず 0x04 を解釈しないため、アプリは decode 済みの `MessageParameters` から `as_slice()` 経由で読む
5. `Subscription::subscriber_rendezvous_timeout_ms` (0086 で削除) も戻さない。`Session::handle_peer_subscribe` は 0x04 を解釈も保持もしない
6. 未知パラメータを `PROTOCOL_VIOLATION` にする §9.20 の処理はそのまま残した

更新したドキュメント:

- `CHANGES.md` の `## develop` にあった「relay 専用の RENDEZVOUS_TIMEOUT parameter (0x04) を削除する」エントリを、受理する方針の `[CHANGE]` エントリに置き換えた (同一未リリース内で実装が入れ替わるため)
- `docs/IMPLEMENTATION.md` の「relay 専用の以下のパラメータも実装しない」から 0x04 を外し、受理するが解釈しないことを明記した
- `skills/shiguredo-moqt/SKILL.md` のパラメータ型定数表に `PARAM_RENDEZVOUS_TIMEOUT` (`0x04`) の行を戻した

テスト:

- `tests/test_message.rs` の `subscribe_with_unhandled_type_0x04_is_rejected` を `subscribe_with_rendezvous_timeout_is_accepted` に置き換えた。encode と decode の両方が成功し、decode 後も 0x04 が `MessageParameters` に残ることを固定する
- `tests/test_message.rs` に `subscribe_with_undefined_parameter_type_is_rejected` を追加し、Table 13 に無い型 (0x7F) が encode で `ProtocolViolation` になることを固定した
- `tests/test_session/request_stream.rs` に `recv_subscribe_with_rendezvous_timeout_is_accepted` を追加し、0x04 を含む SUBSCRIBE を `Session::recv_request` に渡してもセッションが `Established` のままであることを固定した
