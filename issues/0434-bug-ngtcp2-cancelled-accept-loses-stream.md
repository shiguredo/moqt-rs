# ngtcp2 バックエンドで受理待ちのキャンセルにより受理済みストリームが失われる

- Created: 2026-11-15
- Completed: 2026-11-15
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)

## 目的

`crates/tokio-moq/src/quic/ngtcp2.rs` のストリーム受理で、受理待ちの future が
`select!` / `timeout` で破棄されると、受理済みのストリームが失われる。失われたストリームの
データは再送されないため、購読の data stream が永久に届かず、上位は待ち時間の上限まで
気づけない。受理済みストリームを future の破棄で失わないようにする。

## 現状

- ドライバは受理したストリームを `oneshot::Sender` でアプリの受理待ちへ渡している
  (`Command::AcceptUni` / `Command::AcceptBidi` と `ConnectionEvent::StreamOpened` の経路)。
- `oneshot::Sender::send` は receiver が生きていれば成功する。しかし送信が成功した直後に
  receiver が drop されると、oneshot に格納された `RecvStream` はそのまま破棄される。
- example の main ループは受理を `select!` の分岐に置いており、他の分岐 (tick、shutdown、
  制御イベント) が先に完了すると受理 future は破棄される。このとき、ドライバの送信が
  成功した後・アプリが次の poll で取り出す前の窓に入ると、ストリームが失われる。
- `RecvStream` は `Drop` を実装していないため、失われたストリームは開いたまま残る。
  ドライバの `recv_txs` には送信側が残り、receiver だけが消えるため、以降の
  `RecvEvent::Data` は送信に失敗して黙って捨てられる。ピアはデータを送り続けるが、
  アプリには永久に届かない。
- 影響: `quic-ngtcp2` ビルドで example の統合テストが稀に失敗する。観測は次のとおり。
  - `cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests`
    を 40 回実行して 5 回失敗 (12.5%)。落ちるテストは固定ではなく、
    `run_completes_from_catalog_to_shutdown_with_an_audio_object` /
    `run_continues_the_subscription_after_a_failed_fetch_migration` /
    `run_reissues_the_audio_request_stream_without_gaps` /
    `run_migrates_the_audio_request_to_a_side_session_without_duplicates` などで、
    共通の症状は「音声 Object が届かない」。
  - 単一テストを 12 プロセス並列で 40 ラウンド回すと 480 回中 16 回失敗 (3.3%)。
  - `quic-s2n` / `quic-tobi` では failure を観測していない。
- 受理待ちの「送信に失敗したとき」の窓は、送れなかったストリームを次の待ち手へ渡す形で
  既に塞いである。ただし「送信に成功した直後に receiver が drop される」窓は残っている。
- 同じ窓は datagram の受信待ちにもあるが、本 issue はストリーム受理に限る (datagram は別 issue)。

## 設計方針

- ドライバとアプリで共有する待ち行列 (`StreamQueue<T>`) を導入し、受理済みストリームは
  アプリが取り出した時点で初めてキューから消す。受理 future を破棄してもキューからは
  何も取り出さないため、ストリームは失われない。
- 起床は `tokio::sync::Notify` で行う。通知は合図に過ぎず、値の有無はキューが持つため、
  通知を取りこぼしても次の確認で取り出せる。`notified().enable()` をキューの確認より先に
  呼び、確認直後の push でも起こされるようにする。
- 接続が閉じたときはキューの閉鎖状態を立てて待ち手を起こす。閉鎖後に取り出した
  ストリームの読み出しは接続終了で失敗する (`finish()` の既存挙動と同じ)。
- 受理 API (`accept_receive_stream` / `accept_bidirectional_stream`) はコマンドを送らず
  キューから取り出すだけにする。ドライバを生かしておくための送信側は
  `StreamAcceptor` が保持する。
- 回帰テストは 2 種類置く。
  - `StreamQueue` の単体テスト: 待ち手が破棄されても値が残ること、閉鎖で待ちが終わることを
    決定的に固定する。
  - 統合テスト: peer が stream を開く操作と、短い `timeout` で受理 future を破棄する操作を
    繰り返し、最後にすべて受理できることを確認する。競合の窓は狭いため、反復回数で
    検出力を確保する。
- 変異確認では、受理の受け渡しを oneshot に戻した状態で統合テストが落ちることを確認し、
  そのときの発生率を記録する。

## 完了条件

- 受理待ちの future を破棄しても、受理済みストリームが失われないこと。
- `StreamQueue` の単体テストと、反復する統合テストがあること。
- `cargo test -p tokio-moq --lib` が通ること。
- `cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests`
  を 20 回以上実行して失敗しないこと。
- `cargo test -p moq-pub --lib --no-default-features --features quic-ngtcp2 peer_integration_tests`
  が通ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

受理済みストリームをドライバからアプリへ `oneshot` で直接渡すのをやめ、両者で共有する待ち行列
(`StreamQueue<T>`) に変えた。ストリームはアプリが取り出した時点で初めてキューから消えるため、
受理待ちの future が `select!` / `timeout` で破棄されても失われない。

- `crates/tokio-moq/src/quic/ngtcp2.rs` に `StreamQueue<T>` を追加した。`Mutex<VecDeque<T>>` と
  `tokio::sync::Notify` で構成し、`notified().enable()` をキューの確認より先に呼ぶことで、
  確認直後に積まれても取りこぼさない。通知は起床の合図だけで、値の有無はキューが持つ。
- `StreamAcceptor` は `Command::AcceptUni` / `AcceptBidi` を送らず、共有キューから取り出す
  (`StreamQueue::recv`)。`accept_receive_stream` / `accept_bidirectional_stream` の doc に
  キャンセル安全であることを明記した。
- 接続終了時は `StreamQueue::close()` で待ち手を起こす。閉鎖後に取り出したストリームの
  読み出しは接続終了で失敗する (`finish()` の既存挙動と同じ)。
- `StreamAcceptor` はドライバを生かしておくための `mpsc::Sender<Command>` を保持する。

回帰テストを 2 種類追加した。

- `StreamQueue` の単体テスト 4 件 (`crates/tokio-moq/src/quic/ngtcp2.rs`)。破棄された待ち手が
  値を消費しないこと、積まれた順に取り出せること、閉鎖で待ちが終わること、閉鎖後も積まれた
  値を取り出せることを固定する。
- `crates/tokio-moq/src/peer_harness.rs` の
  `dropping_a_pending_accept_does_not_lose_the_delivered_stream`。受理待ちを 1 回だけ poll して
  待ち手として登録し、ドライバが引き渡したあとに値を受け取らず破棄する。その後の受理で
  取り出せることを確認する。noop waker で 1 回だけ poll する helper (`poll_once`) を使う。

変異確認は次のとおり。修正を戻すと、`dropping_a_pending_accept_does_not_lose_the_delivered_stream`
が 3 回実行して 3 回とも失敗する (決定的に検出できる)。修正後は 3 回とも成功する。
修正前の example 統合テストは 40 回中 5 回失敗していたが、修正後は
`cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests`
を 20 回実行して 0 回失敗、`moq-pub` 側も 5 回実行して 0 回失敗だった。

検証: `cargo test -p tokio-moq --lib` (298 件) /
`cargo test -p tokio-moq --lib --no-default-features --features quic-ngtcp2` (302 件) /
`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo clippy -p tokio-moq --all-targets --no-default-features --features quic-ngtcp2 -- -D warnings` /
`cargo fmt --all -- --check`。

残る課題 (別 issue で扱う):

- open 待ちの future がキャンセルされると、開いたストリームのハンドルが失われ、FIN も
  RESET_STREAM も送られないまま残る。
- datagram の受信待ちも `oneshot` で渡しており、同じ窓で datagram が失われる。
