# example の publisher が peer からの REQUEST_UPDATE に応答しない

- Created: 2026-09-17
- Completed: 2026-09-17
- Branch: feature/fix-example-request-update-response
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) は「The receiver of a REQUEST_UPDATE MUST respond with exactly one REQUEST_OK or REQUEST_ERROR message indicating if the update was successful, unless it is coalescing failed updates」と定める。example の publisher は REQUEST_UPDATE を受信しても何も返さないため、この MUST を満たしていない。

relay 経由の実測では、subscriber example が送った REQUEST_UPDATE が publisher example まで転送されても応答が返らず、subscriber 側の control message 応答待ち deadline (30 秒) が満了してセッションが CONTROL_MESSAGE_TIMEOUT (0x11) で閉じる。publisher / subscriber を 30 秒以上動かすと必ずセッションが落ちるため、example の動作確認が成立しない。

## 現状

- `shiguredo_moqt` の `SessionEvent::RequestUpdateReceived` は「アプリは `send_request_ok` / `send_request_error` で応答する」と doc に明記されているが、example の共有トランスポート層 `examples/moqt-transport/src/moqt_client.rs` の session event ループは `RequestUpdateReceived` を「特別な処理を行わない」腕にまとめており、応答もアプリへの通知も行わない
- `examples/moqt-publisher/src/pipeline.rs` の notable event ループは `ClientEvent::Request` (SUBSCRIBE / FETCH) を `serve_peer_request` に渡して `send_subscribe_ok` / `send_fetch_ok` で応答するが、REQUEST_UPDATE に対応する `ClientEvent` が無いため応答経路が存在しない
- `examples/moqt-subscriber/src/pipeline.rs` は確認用に REQUEST_UPDATE を送る (`send_request_update` で video subscription の SUBSCRIBER_PRIORITY を変更するサンプル) ため、publisher が応答しないことがそのまま停止として現れる

relay を介した実測 (publisher は `moqt://127.0.0.1:4433` に PUBLISH、subscriber は同 relay に SUBSCRIBE):

- subscriber は 18:12:33.917 に REQUEST_UPDATE を送信し、その直後に control message 応答待ち deadline (30 秒) を開始する
- relay は REQUEST_UPDATE を publisher へ転送し、publisher からの応答を待って下流へ返す (relay の coordinator の REQUEST_UPDATE 上流転送)。publisher が応答しないため下流へは何も返らない
- 18:13:03.928 (送信の 30.0 秒後) に subscriber が `control message timeout expired` (0x11) でセッションを閉じる。データ (video 799 frames / audio 1500 chunks) はその直前まで正常に受信していた

## 設計方針

- `examples/moqt-transport/src/moqt_client.rs` に REQUEST_UPDATE 受信をアプリへ通知する `ClientEvent` を追加し、`MoqtClient` に `send_request_ok` のラッパーを追加する。応答の要否をアプリが判断できるようにし、黙殺をやめる
- `examples/moqt-publisher/src/pipeline.rs` で受信した REQUEST_UPDATE に REQUEST_OK を返す。パラメータの検証 (scope / Range Filter / FORWARD 値域) は `shiguredo_moqt` の session 層が受信時に済ませており、通過した更新は購読状態へ反映されるため、example はそのまま受理する
- `examples/moqt-subscriber/src/pipeline.rs` でも同じイベントを扱い、受信した場合は同様に REQUEST_OK を返す (subscriber が PUBLISH を受けた購読で publisher 側から更新が来る組合せがあるため、片面だけの配線にしない)
- 応答漏れが再発しないよう、example のイベントループで REQUEST_UPDATE を無視する腕を残さない

## 完了条件

- publisher example が peer から受信した REQUEST_UPDATE に REQUEST_OK を返すこと
- subscriber example が同じイベントで REQUEST_OK を返すこと
- publisher / subscriber を 30 秒以上動かしても CONTROL_MESSAGE_TIMEOUT でセッションが閉じないこと (relay 経由の実測)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `CHANGES.md` の `## develop` にエントリが追加されていること

## 参照

- draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE)
- draft-ietf-moq-transport-21 §9.3 (REQUEST_OK)
- draft-ietf-moq-transport-21 §12.2 (Session Termination Codes: CONTROL_MESSAGE_TIMEOUT)

## 解決方法

- `examples/moqt-transport/src/moqt_client.rs`
  - `ClientEvent` に `RequestUpdate(IncomingRequestUpdate)` を追加し、`SessionEvent::RequestUpdateReceived` を無視する腕から外してアプリへ引き渡すようにした
  - `MoqtClient::send_request_ok(request_id)` を追加した (空のパラメータと Track Properties で REQUEST_OK を送る)
  - `next_event` は未処理の REQUEST_UPDATE を要求より先に返す。応答を待たせると peer の control message 応答待ちを招くためである
- `examples/moqt-publisher/src/pipeline.rs` / `examples/moqt-subscriber/src/pipeline.rs`
  - 受信した REQUEST_UPDATE に REQUEST_OK を返す腕を追加した。パラメータの検証と購読状態への反映は `shiguredo_moqt` の session 層が受信時に済ませているため、example はそのまま受理する
- 検証
  - `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した
  - relay (develop) 経由で publisher / subscriber を 60 秒動作させ、publisher が REQUEST_UPDATE を受信して応答し、subscriber が CONTROL_MESSAGE_TIMEOUT で閉じずに video 1535 frames / audio 2804 chunks を受信し続けることを確認した (修正前は REQUEST_UPDATE 送信の 30.0 秒後に 0x11 で閉じていた)
