# example の stop_sending が実際に STOP_SENDING を送るようにする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-stop-sending

## 目的

`examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::stop_sending` を名前とログに沿った挙動にする。購読停止時に peer へ cancel を伝える。

## 現状

`MoqtClient::stop_sending` は `session.stop_sending(request_id)` を呼ぶだけで、QUIC の bidi request stream に STOP_SENDING を送出しない。ライブラリ側の契約 (`src/session/subscription/recv.rs` の doc) は「I/O 層が QUIC STOP_SENDING を実行済みであり、session 層はイベントを発行しない」としている。にもかかわらず wrapper は `tracing::info!("Sent
STOP_SENDING ...")` と出力する。

subscriber は停止時に `client.stop_sending(rid)` を呼ぶが、peer へ cancel は伝わらない。後続の `client.close(0, "")` で接続ごと閉じるため顕在化しにくいだけである。

## 設計方針

bidi 受信方向を所有するタスクへ停止指示を渡し、`ReceiveStream` に対して実際に STOP_SENDING を送出する。少なくともログを「state を Terminated にしただけ」と実態に合わせる。

## 完了条件

- `stop_sending` が QUIC / WebTransport の bidi request stream に STOP_SENDING を送出すること
- 送出できない場合のログが実態と一致すること
- example のビルドが維持されること
