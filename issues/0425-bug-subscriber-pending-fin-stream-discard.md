# moq-sub が購読の確立前に FIN まで届いた Subgroup ストリームを捨てない

- Created: 2026-11-10
- Completed: 2026-11-10
- Branch: feature/fix-subscriber-pending-fin-stream-discard
- Polished: 2026-11-10

## 目的

moq-sub は Track Alias が未確立の Subgroup ストリームを短時間保持する (draft-ietf-moq-transport-23 §3.1.3.1 の "buffer it briefly")。
しかし保持中に自ストリームの FIN が届くと、保持したチャンクがすべて揃っているのに abort して捨てる。そのため SUBSCRIBE_OK より先に FIN まで届いた短い Subgroup は、直後に購読が確立しても失われる。保持の目的 (制御メッセージとの順序入れ替わりへの対処) を FIN 済みのストリームでも果たす。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `register_subgroup_header` は、保持中に `receive_registered_stream_data` が `None` (FIN / RESET_STREAM / Session の STOP_SENDING 指示) を返すと `note_end_of_stream(id)` で自分の entry に終端を通知する。
  次のループで `take_ready_for` が `PendingNotifyReason::EndOfStream` を返すと、debug ログを出して `abort_subgroup_stream` を呼び、`false` を返す (保持したチャンクは decoder へ渡らない)。
- `receive_registered_stream_data_outcome` は `RecvChunk::End` を観測すると同時に `data_plane.recv_data_stream_closed` を呼ぶため、FIN を読んだ時点で header 未受理の stream は Session の会計 (`data_streams.incoming`) から除去される。その後の `recv_subgroup_header` 再試行は `unknown stream id` の `Err` になり、保持ループは確立を検出できずに破棄へ落ちる。FIN を保持し続けるには、FIN 観測時の Session への終端通知を遅らせる必要がある。
- `src/pending_subgroup_buffer.rs` の `note_end_of_stream` の doc は「保持しているチャンクは捨てない (所有者が読み直せるように残す)」とし、既に理由が確定した entry と存在しない識別子には何もしない。バッファの通知は「最初の 1 回だけ有効」の契約で、後から別の理由で `note_*` を呼んでも理由は上書きしない (`notify_alias` は理由未設定の entry だけを更新する)。
- moq-pub は音声を既定で OBJECT_DATAGRAM で配信し、`--audio-subgroup` 指定時のみ 1 フレームごとに `SubgroupWriter::new` → `write_object` → `finish` の短い Subgroup stream を使う。この経路の音声は FIN まで一度に届きやすい。
- 各 QUIC バックエンド (`ngtcp2` / `s2n` / `tobi`) は、FIN を読み進めたあとに再度読んでも `End(Fin)` が観測できる (ngtcp2 は transport wrapper が終端を記憶して冪等に返し、s2n / tobi は下層が冪等である)。RESET_STREAM の再読みはバックエンド差があるため、本設計で再読みして Session へ通知するのは確立後の FIN だけである。
- 移植元の moqt-js (外部リポジトリ) も FIN の時点で subscriber が居なければ entry を破棄する (`src/session/dataStreamIncoming.ts`)。本 issue はこの挙動を変える。

## 設計方針

- FIN で Session への終端通知を遅らせる。保留ループでの読み出しは、`wait_data_stream_stop_sending` と transport 直読みを select する既存の `receive_registered_stream_data_outcome` と同型で、`End` を観測しても `recv_data_stream_closed` を呼ばない読み出しに替える。FIN と他の終端の区別は既存の `RegisteredStreamData::End` (`RequestStreamEnd`) と `StoppedBySession` が提供するため、区別のための新 API は作らない。
- FIN 観測後は `note_end_of_stream(id)` を呼ばない。「最初の 1 回だけ有効」契約により先に `EndOfStream` を設定すると、後から `note_subscriber` で `Subscriber` へ理由を切り替えられないためである。entry は理由未設定のまま保持し、期限は保持の開始時刻からの `timeout_us` のまま使う。FIN 観測後は stream を読まない (読む内容が無く、終端した stream は FIN を即時に再配信するため読み出しを select に入れると busy loop になる)。FIN 済みの待機でも、entry の通知 (`Timeout` / `OverflowPerStream` / `OverflowPerSession` / `SessionClose`。現行の pipeline には `SessionClose` を発火する経路は存在しない) の取得起点 (`take_ready_for`) と STOP_SENDING 指示の監視 (`wait_data_stream_stop_sending`) を失わないこと。busy loop は作らず、再試行の周期をどう満たすかは実装の自由とする (`recv_subgroup_header` の再試行は継続する)。
- 確立を検出したら (`recv_subgroup_header` の再試行が `Accepted`)、`note_subscriber(header.track_alias)` を呼んで次のループの `take_ready_for` (理由は `Subscriber`) で保持チャンクを decoder へ戻し、処理を続ける。`Subscriber` 理由で引き取ったときは、自分の `recv_subgroup_header` の再試行で `Accepted` になったことを確認してからチャンクを戻す (この確認は 0426 と同じ規則であり、FIN 済み保持の `Subscriber` 引き取りでも同様に行う)。確立後は decode ループが `End(Fin)` を再読みし、既存の `receive_registered_stream_data_outcome` 経路で初めて `recv_data_stream_closed` を呼んで Session の会計を正しく閉じる。FIN 済みの stream は Session に登録されたままなので二重通知は起こらず、`unknown stream id` の警告も出ない。mid-object FIN は既存の `mid_object_fin_should_report` / `report_mid_object_fin_if_needed` (FIN かつ `UnexpectedEof`) がそのまま適用されるため、Session 側 (`src/session/data.rs`) の変更は不要である。
- RESET_STREAM と Session の STOP_SENDING 指示による終端は、ストリームの内容が欠けている可能性があるため従来どおり abort する (`note_end_of_stream` → `EndOfStream` 経由の既存の分岐を維持する)。読み出しエラーは従来どおり entry を `remove(id)` して破棄する。abort 経路の `abort_subgroup_stream` は `send_data_stream_stop_sending` で会計を閉じる (FIN を Session に通知していないため stream は既知 id として扱え、`unknown stream id` の `Err` は出ない)。abort 前後の読み捨て (drain) には終端通知を遅延する読み出しを使い、`recv_data_stream_closed` は FIN について成功・破棄いずれの経路でも 1 回以内に限る。
- 期限 (Timeout) / `OverflowPerStream` / `OverflowPerSession` / `SessionClose` 理由の取扱は現行どおりとし、FIN 保持の導入で変えない。
- moqt-js (外部リポジトリ) との挙動の違いは pipeline の doc に理由とともに書く。

## 変更対象

- `crates/moq-sub/src/pipeline.rs` (終端通知を遅延する読み出しの導入、`register_subgroup_header` の FIN 保持と abort / 確立分岐、`Subscriber` 分岐の自 header 受理確認、`register_subgroup_header` とモジュール doc)
- 同一ファイルの `mod tests` (保持 / abort 判定の純関数テスト)
- Session 側 (`src/session/data.rs`) は変更しない

## 完了条件

- 購読の確立前に FIN まで届いた Subgroup ストリームが、期限内に購読が確立すれば decoder へ届けられること。FIN 済みの保持でも alias 単位の `note_subscriber` 通知で起こり、自分の `recv_subgroup_header` が `Accepted` になったことを確認してからチャンクを渡すること。
- FIN は Session の会計に 1 回だけ通知されること。FIN 観測時 (保持中) には `recv_data_stream_closed` を呼ばず、確立後の decode ループの `End(Fin)` で通知すること。FIN 保持からの破棄経路 (RESET / STOP_SENDING 指示 / 期限切れ / Overflow / SessionClose) では `recv_data_stream_closed` を呼ばず、`abort_subgroup_stream` の `send_data_stream_stop_sending` で会計が閉じられること (この経路では RESET の観測時にも Session 通知を行わず、stream が既知 id として扱えるため `unknown stream id` の `Err` の警告は出ないこと)。
- RESET_STREAM / STOP_SENDING 指示による終端と、期限切れでは従来どおり破棄されること。
- 保持 / abort 判定の純関数を `crates/moq-sub/src/pipeline.rs` の `mod tests` に切り出し、FIN (保持: `recv_data_stream_closed` を呼ばない) / RESET_STREAM (abort) / STOP_SENDING 指示 (abort) の分岐を単体テストで固定すること。経路の駆動は `transport::RecvStream` と `DataPlaneHandle` を組み立てる基盤が無く、モック・スタブも使えない (AGENTS.md が禁止) ためコード上で確認すること。
- moqt-js (外部リポジトリ) との挙動の違いは pipeline の doc に理由とともに書かれていること。`register_subgroup_header` の doc が FIN の遅延通知と保持解除の契約を説明していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `crates/moq-sub/src/pipeline.rs` に保持経路専用の読み出し `receive_pending_hold_stream_outcome` を追加した。既存の `receive_registered_stream_data_outcome` と同じ select 構造で、`RecvChunk::End` を観測しても `recv_data_stream_closed` を呼ばない。FIN の Session 会計通知は保持の解除後の decode ループが 1 度だけ行う。
- `register_subgroup_header` の保持ループの読み出しを上記ヘルパに替え、FIN を観測したら entry を理由未設定のまま保持して読み出しを止めた。期限・`Timeout` / `Overflow` / `SessionClose` 理由の取得起点 (`take_ready_for`) と STOP_SENDING 指示の監視 (`wait_data_stream_stop_sending`) を維持し、確立待ちは自分の `recv_subgroup_header` 再試行と別 task の `note_subscriber` 通知で行う。
- RESET_STREAM / STOP_SENDING 指示による終端は従来どおり `note_end_of_stream` → 終端分岐 (abort) で破棄する。abort 経路の読み捨ては新設の `drain_registered_stream_quietly` (会計通知を遅らせる読み出し) に統一し、FIN について `recv_data_stream_closed` は成功・破棄いずれの経路でも 1 回以内に限る (unknown stream id の警告は出ない)。
- 0426 で導入した task ローカル保持 (`LocalPendingHold` + `continue_pending_subscriber_locally`) を FIN 済み保持に拡張した (`already_fin` を追加)。FIN を観測した保持も、読み出しを止めて自分の再試行で確立を待ち、確立したら保持チャンクを decoder へ戻して処理を続ける (poll 間隔 `LOCAL_PENDING_HOLD_POLL_INTERVAL_US` で待機し、busy loop を作らない)。
- 保持 / abort 判定の純関数 `pending_hold_end_decision` を追加し、`pipeline::tests::pending_hold_end_decision_tests` に FIN (保持) / RESET_STREAM (abort) / STOP_SENDING 指示 (abort) の分岐を固定した。経路の駆動はコード上で確認した (AGENTS.md がモック・スタブを禁止しているため)。
- moqt-js (外部リポジトリ) との挙動の違いと FIN の遅延通知、保持解除の契約を `register_subgroup_header` と `continue_pending_subscriber_locally` の doc に記載した。
- Session 側 (`src/session/data.rs`) と `src/pending_subgroup_buffer.rs` は変更しない (完了条件のとおり)。
