# example の stop_sending が実際に STOP_SENDING を送るようにする

- Created: 2026-09-10
- Completed: 2026-09-11
- Branch: feature/fix-example-stop-sending
- Polished: 2026-09-11

## 目的

`examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::stop_sending` を名前とログに沿った挙動にする。購読停止時に peer へ cancel を伝える。

## 現状

`MoqtClient::stop_sending` は `session.stop_sending(request_id)` を呼ぶだけで、QUIC の bidi request stream に STOP_SENDING を送出しない。ライブラリ側の契約 (`src/session/subscription/recv.rs` の doc) は「I/O 層が QUIC STOP_SENDING を実行済みであり、session 層はイベントを発行しない」としている。にもかかわらず wrapper は `tracing::info!("Sent
STOP_SENDING ...")` と出力する。

subscriber は停止時に `client.stop_sending(rid)` を呼ぶが、peer へ cancel は伝わらない。後続の `client.close(0, "")` で接続ごと閉じるため顕在化しにくいだけである。

## 設計方針

- `MoqtClient::stop_sending` は `Session::stop_sending` で subscription state を Terminated にしたうえで、bidi request stream の受信半に STOP_SENDING を送出する。`Session::stop_sending` がエラーを返した場合は送出せず、エラー理由をログする。
- 成功ログ `Sent STOP_SENDING` は送出完了後にのみ出す。受信タスクが既に終了している等で送出できない場合は、未送出であることと理由をログする。
- bidi request stream の受信半は `spawn_bidi_recv_task` のタスクが所有するため、メインループから受信タスクへ停止指示を渡すチャネルを追加し、request_id に対応するストリームへ指示できるようにする。
- transport 抽象 (`RecvStream` / `WtRecvStream`) に STOP_SENDING 送出 API を追加する。QUIC / WebTransport はどちらも内部で `s2n_quic::stream::ReceiveStream::stop_sending` を呼べる。
- STOP_SENDING の error code は draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes) の CANCELLED (0x1) を使う。

## 完了条件

- `MoqtClient::stop_sending` が bidi request stream に QUIC STOP_SENDING を送出すること (QUIC / WebTransport の両 transport 実装に送出経路があること)
- `Sent STOP_SENDING` のログが送出完了後にのみ出ること。送出できない場合は未送出であることと理由がログに出ること
- メインループから bidi 受信タスクへ request_id 単位の停止指示を渡す経路が追加されていること
- example のビルドが維持されること
- subscriber の停止時に peer が request stream の停止を観測できることを確認する手順が、issue または example のコードコメントに記載されていること (標準フローは直後に connection close するため、確認時は close を遅らせる)

## 解決方法

`MoqtClient::stop_sending` が bidi request stream に実際の STOP_SENDING を送出するようにした。

- `RecvStream` / `WtRecvStream` に STOP_SENDING 送出 API を追加した (QUIC / WebTransport 両対応)。
- bidi 受信タスクへ request_id 単位で停止指示を渡すチャネルを追加し、タスクは指示を受けると受信半に STOP_SENDING を送出して結果を oneshot で返す。
- stop_sending は Session の状態遷移後、送信方向の RESET_STREAM (draft-ietf-moq-transport-21 §6.4.2.3)、回収対象への登録、停止指示、ack 待ち (1 秒タイムアウト) の順で行う。`Sent STOP_SENDING` は送出 API 呼び出し成功後のみログし、失敗時は理由を警告ログに残す。
- 手動確認手順を doc コメントに記載した。
- `CHANGES.md` の `[FIX]` にエントリを追加した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
