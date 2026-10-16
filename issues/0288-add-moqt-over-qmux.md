# examples の moq-pub / moq-sub に QMux 経由の接続を追加する

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-over-qmux
- Polished: {YYYY-MM-DD}
- Updated: 2026-10-16

## 目的

UDP が使えない環境でも MOQT を試せるように、順序保証された双方向バイトストリーム (TLS over TCP) の上で QUIC version 1 のストリーム操作と DATAGRAM 操作を提供する QMux (`shiguredo/qmux-rs`) を、examples の接続経路として追加する。`--transport qmux` で moq-pub / moq-sub が QMux 経由で publish / subscribe できる状態にする。

QMux の実装はまだタグが無く、準拠する draft-ietf-quic-qmux-02 (2026-07-06、provisional なコードポイントを含む) も、QMux 上の MOQT を扱う `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` (rev 00、2026-02-28 発行、Expires 2026-09-01 で既に失効) も落ち着いていない。このため PR は `[WIP]` のままにし、qmux-rs にタグが付いて依存を固定できるようになるまでマージしない。

実装は `feature/add-moqt-over-qmux` ブランチで行う (CODEBASE.md の「Pull-Request やブランチを作らず develop にコミットしていくこと」に反するが、本 issue はユーザーの指示による例外とする)。

## 現状

- examples の接続経路は `examples/tokio-moq/src/lib.rs` の `Transport` が `Quic` / `WtH3` / `WtH2` の 3 つで、`Transport::parse` / `as_str` / `resolve_transport` が `--transport` と `--url` の `msf` fragment の `connection` (`q` / `wt`) から解決する。既定は `quic`。
- 接続確立は `examples/tokio-moq/src/lib.rs` の `connect` が `quic` / `webtransport_h3` / `webtransport_h2` の各モジュールへ分岐し、`examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::establish_quic` / `establish_wt` / `establish_wt_h2` が SETUP までを終える。
- 経路ごとの差は `examples/tokio-moq/src/transport.rs` の `SendStream` / `RecvStream` / `StreamHandle` / `StreamAcceptor` / `BidiStreamAcceptor` が吸収する。
- 本体 `shiguredo_moqt::session::types::Transport` は `Quic` / `WebTransport` の 2 つで、`src/session/core.rs` の `validate_setup_role_transport` が WebTransport のときだけ AUTHORITY / PATH を拒否する。
- qmux-rs の develop は `ffca089` (2026-10-16)。`be936f3` (2026-10-14) から `520bced` (ピアの STOP_SENDING 通知 API)、`7f3e291` (accept 系と DATAGRAM 受信の `&self` 化)、`ffca089` (接続単位の STOP_SENDING 観測 API) が入った。
- qmux-rs はタグが 0 件で、`shiguredo_qmux` は crates.io に無く (crates.io API が 404 を返す)、`tokio-qmux` は `publish = false` (version 0.0.0) である。依存は git の rev 固定で入れるしかない。
- `tokio_qmux::Connection` は Clone ではないが、`open_*` / `accept_*` / `send_datagram` / `recv_datagram` / `recv_stop_sending` / `close` / `closed` がすべて `&self` であるため、`Arc<Connection>` を複数タスクで共有して `select!` で待てる (accept 系と DATAGRAM の receiver は内部の `tokio::sync::Mutex` で待ちを直列化する)。
- tokio-qmux の crate 直下の再エクスポートは `Connection` / `Error` / `Result` / `RecvStream` / `SendStream` / `StopSendingObserved` である。`Config` は `shiguredo_qmux::config::Config`、`Role` は `shiguredo_qmux::codec::role::Role` を直接指す必要がある。
- `Config::default()` はフロー制御のクレジットがすべて 0 で `max_datagram_frame_size` が `None` (DATAGRAM 無効) であり、`Connection::new` は不正な `Config` で panic する。
- STOP_SENDING は tokio-qmux がストリーム単位 (`SendStream::stopped`) と接続単位 (`Connection::recv_stop_sending` が返す `StopSendingObserved { id, error_code }`) の両方へ通知する。`write` / `finish` / `reset` は通知済みなら `Error::Stopped` を返す。
- moqt-rs の QUIC 経路は s2n-quic の `ConnectionObserver` で `Frame::StopSending` を観測し、接続単位の `StopSendingObserved` を MOQT 層へ渡している。
- 仕様面では draft-ietf-moq-transport-22 §6.2 が「TCP など他のトランスポート上で直接動かす方法は定義しない」と明記し、QMux に触れない。QMux 上の MOQT を扱う individual draft は §10.1 で MOQT draft-16 (2026-01) と draft-opik-quic-qmux-00 (2025) を参照しており、QMux の参照は 2 世代古い。
- control stream の対応は draft-ietf-moq-transport-22 §6.4.1 の単方向ストリーム (stream type 0x2F00) が現行で、individual draft §4.2 の「双方向 stream 0 を MUST」とは矛盾する。
- ALPN は両者で食い違う。draft-ietf-quic-qmux-02 §8.1 は「QMux へのマッピングの識別子は同一プロトコルの QUIC マッピングと異なるものでなければならない (SVCB との互換のため)」と MUST で定める。individual draft §3.2 は「native MOQT over QUIC と同じ識別子を使い、新しい ALPN を定義しない」とする。MOQT 側に QMux 用の登録は無い (draft-ietf-moq-transport-22 §16 の ALPN registry は未整備)。
- QMux の DATAGRAM は TCP 上で多重化されるため信頼・順序どおりに届く (draft-ietf-quic-qmux-02 §1.1)。moq-pub は既定で音声を datagram で送る (`--audio-subgroup` の否定で `audio_datagram` が真) ため、この経路では音声が再送される (draft-ietf-moq-transport-22 §6.2 が WebTransport over HTTP/2 について述べる性質と同じ)。
- `tokio_qmux::Connection::recv_datagram` は 1 件ずつ返し、受信は容量 64 件 / 合計 1 MiB の上限を超えると破棄される。moq-sub の datagram 受信タスクは、`StreamHandle::recv_datagrams` が「取り出し済みの datagram は失われない」性質に依存している (`examples/moq-sub/src/pipeline.rs` のコメント)。この性質は `examples/tokio-moq/src/transport.rs` の doc には書かれておらず、実装と pipeline 側のコメントに依存している。
- QMux の仕様 (draft-ietf-quic-qmux-02) の一次資料が `refs/quic/` に無い。`refs/` で QMux を含むのは `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` だけである。

## 設計方針

- `refs/quic/draft-ietf-quic-qmux-02.txt` を追加する。QMux の節番号を引用する根拠をリポジトリ内に置くためである (同ファイルは qmux-rs の `refs/quic/` にある)。
- 依存は `examples/tokio-moq/Cargo.toml` に `shiguredo_qmux` と `tokio-qmux` を同じ rev の git 依存として追加する (タグが無いためバージョン指定ができない)。本体 `shiguredo_moqt` (no_std、MSRV 1.93) には入れない。qmux-rs の workspace は rust-version 1.95 だが、examples は s2n-quic の都合で既に 1.95 であるため変わらない。
- `examples/tokio-moq/src/qmux.rs` を追加する。`tokio::net::TcpStream::connect` で TCP を張り、`tokio_rustls::TlsConnector` で TLS を確立してから、その TLS ストリームを `tokio_qmux::Connection::new(Role::Client, io, config)` に渡す。
- TLS の組み立ては `webtransport_h2.rs` の `build_tls_client_config`、`--cert` 未指定時の警告は `lib.rs` の `apply_tls_config_h2` に揃える。
- ALPN は individual draft §3.2 に従い、native QUIC と同じ `MOQT_PROTOCOL` (`moqt-22`) を使う。QMux 上の MOQT 専用の識別子は定義せず、下位トランスポートは接続そのもので区別する。
- draft-ietf-quic-qmux-02 §8.1 は「QMux へのマッピングの識別子は同一プロトコルの QUIC マッピングと異なるものでなければならない (SVCB との互換のため)」と MUST で定めるため、この選択は同節から外れる。MOQT 側に QMux 用の登録が無く SVCB による MOQT の探索も定義されていないため interop を優先する。この食い違いと、両 draft が決着したら見直すことをコメントと PR 本文に残す。
- `tokio_qmux::Config` は既定値のままでは使えないため明示する。値は `quic.rs` の `build_limits` が決めている同時実行数と窓 (単方向 stream 10,000 / 双方向 stream 1,000 / 接続 64 MiB / stream 16 MiB) に相当する値を写し、`max_datagram_frame_size` を音声の Object が収まる値にする。
- individual draft §3.3 の推奨値 (接続 1 MiB、per-stream 256 KiB、stream 数 100、max_frame_size 16384) は moq-sub の要求に対して小さく、そのままは使わない。値の根拠をコメントに残す。
- `Arc<tokio_qmux::Connection>` を `StreamHandle::Qmux` / `StreamAcceptor::Qmux` / `BidiStreamAcceptor::Qmux` が共有する。`open_*` / `send_datagram` / `close` は直接呼び、`accept_unidirectional` / `accept_bidirectional` は receiver へ流すタスクから待ち、`closed()` を select して接続終了で待ち続けない (`wait_until_terminated` と同じ役割)。
- datagram は `qmux.rs` のタスクが `recv_datagram` を回して自前のバッファへ移し、`recv_datagrams` は (1) バッファが空なら 1 件届くまで待ち、(2) 溜まっている分を await せずにまとめて返す。tokio-qmux の受信チャネルは 64 件 / 1 MiB で溢れた分が破棄されるため、早く引き取る方が音声の取りこぼしが減る。
- STOP_SENDING は `Connection::recv_stop_sending` を待つタスクが `crate::quic::StopSendingObserved { stream_id: id.as_u64(), error_code }` に詰め替え、QUIC 経路と同じ `StopSendingReceiver` のチャネルへ流す。`MoqtClient::handle_stop_sending` と `establish_quic` と同じ結線を `establish_qmux` でも行う。`SendStream::stopped` は送信と同時に await できないため使わない。
- `Transport` に `Qmux` を足し、`--transport qmux` で選べるようにする。MSF fragment の `connection` は `q` / `wt` しか定義しないため、`connection=q` と `--transport qmux` は矛盾としてエラーにする (`resolve_transport` の既存の腕で自動的に成立することをテストで固定する)。
- `transport.rs` の 5 つの enum に QMux を足す。`StreamId` は `as_u64` で `u64` に落とし、`read` の `Ok(None)` を `RequestStreamEnd::Fin`、`Error::Reset` を `RequestStreamEnd::Reset { error_code: Some(code), reliable_size: None }` に対応させる (QMux は RESET_STREAM_AT を実装しないため reliable_size は常に `None`)。
- STOP_SENDING / RESET_STREAM のコード空間は QUIC version 1 のものを多重化するため remap せず、`StopSendingTransport::Qmux` を追加して `Quic` と同じ扱いにする。
- ストリームの対応は QMux が QUIC version 1 のストリームを多重化する性質に従い 1 対 1 にする。control stream は単方向 (stream type 0x2F00)、request stream は双方向とする。individual draft §4.2 の「双方向 stream 0」は draft-ietf-moq-transport-22 §6.4.1 と矛盾するため採らない。採らない理由をコメントに残す。
- 本体ライブラリは `session::types::Transport` に `Qmux` を足すだけにする。`validate_setup_role_transport` は WebTransport だけを拒否の対象にしているため、QMux は native QUIC と同じく AUTHORITY / PATH を載せる。draft-ietf-moq-transport-22 §9.1.1 / §9.1.2 が AUTHORITY / PATH を native QUIC に限るため、この扱いが仕様の範囲外であることをコメントに残す。
- moq-pub / moq-sub の pipeline は、WebTransport 経路と同じく QMux 経路にも experimental である旨の警告ログ (英語) を出す。理由は WebTransport の `RESET_STREAM_AT` とは異なるため、QMux 用の理由 (仕様も実装も流動的であること、datagram が信頼配送になること) を書く。
- テストはモックを使わず、127.0.0.1 の TCP + rustls (rcgen の自己署名証明書) で QMux のサーバー側を立て、`Session::new_server(Transport::Qmux, ...)` を駆動して確かめる (`webtransport_h2.rs` のテストが TLS サーバーを立てているのと同じ方法)。
- `examples/tokio-moq/src/peer_harness.rs` は QUIC 専用であるため、QMux 用のハーネスを追加するかは実装時に決める。
- 実 relay の E2E (`e2e-tests/`) は QMux 経路を終端できる MoQ リレーが確認できていないため対象外とし、`MOQT_E2E_TRANSPORT` に `qmux` は足さない。
- 3 経路を列挙している記述を追随させる。対象は `examples/tokio-moq/README.md` のトランスポート表と「3 経路」、リポジトリ直下 `README.md`、`examples/moq-pub/README.md` / `examples/moq-sub/README.md` の `--transport` の行である。
- あわせて `examples/moq-pub/src/cli.rs` / `examples/moq-sub/src/cli.rs` の `.doc(...)`、`examples/tokio-moq/src/lib.rs` の crate ドキュメント、`examples/moq-pub/src/main.rs` / `examples/moq-sub/src/main.rs` の doc コメント、`e2e-tests/README.md` の環境変数表も追随させる。
- PR はタイトル先頭に `[WIP]` を付けて作成し、qmux-rs にタグが付くまでマージしない。issue はマージまで open のままにする。

## 完了条件

- `--transport qmux` で moq-pub / moq-sub が QMux 経由で接続し、SETUP と PUBLISH / SUBSCRIBE が成立すること (TCP + TLS + QMux のテスト peer で確認する)。
- 映像 (subgroup stream) と音声 (datagram) の双方が QMux 経由で届くこと。
- ピアの STOP_SENDING が MOQT 層へ届き、該当する送信ストリームの終端として扱われること (`Connection::recv_stop_sending` を使う)。
- `Transport::parse` / `as_str` の逆写像、`resolve_transport` の `connection=q` との矛盾の検出、`StopSendingTransport::Qmux` が remap しないことがテストで固定されていること。
- 3 経路を列挙している README と doc コメントが 4 経路に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- PR が `[WIP]` 付きで作成され、依存する qmux-rs の rev、タグが付いたら依存を切り替えること、QMux 経路に残る制約 (datagram が信頼配送になること、RESET_STREAM_AT 未実装のため `reliable_size` が常に `None` になること) が PR 本文に書かれていること。
