# examples の moq-pub / moq-sub に QMux 経由の接続を追加する

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-over-qmux
- Polished: {YYYY-MM-DD}

## 目的

UDP が使えない環境でも MOQT を試せるように、順序保証された双方向バイトストリーム (TLS over TCP) の上で QUIC version 1 のストリーム操作と DATAGRAM 操作を提供する QMux (`shiguredo/qmux-rs`) を、examples の接続経路として追加する。`--transport qmux` で moq-pub / moq-sub が QMux 経由で publish / subscribe できる状態にする。

QMux の実装はまだタグが無く、準拠する draft-ietf-quic-qmux-02 も、QMux 上の MOQT を扱う individual draft も流動的である。このため PR は `[WIP]` のままにし、qmux-rs にタグが付いて依存を固定できるようになるまでマージしない。実装は `feature/add-moqt-over-qmux` ブランチで行う (CODEBASE.md の「Pull-Request やブランチを作らず develop にコミットしていくこと」に反するが、本 issue はユーザーの指示による例外とする)。

## 現状

- examples の接続経路は `examples/tokio-moq/src/lib.rs` の `Transport` が `Quic` / `WtH3` / `WtH2` の 3 つで、`Transport::parse` / `as_str` / `resolve_transport` が `--transport` と `--url` の `msf` fragment の `connection` (`q` / `wt`) から解決する。既定は `quic`。
- 接続確立は `examples/tokio-moq/src/lib.rs` の `connect` が `quic` / `webtransport_h3` / `webtransport_h2` の各モジュールへ分岐し、`examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::establish_quic` / `establish_wt` / `establish_wt_h2` が SETUP までを終える。
- 経路ごとの差は `examples/tokio-moq/src/transport.rs` の `SendStream` / `RecvStream` / `StreamHandle` / `StreamAcceptor` / `BidiStreamAcceptor` が吸収する。
- 本体 `shiguredo_moqt::session::types::Transport` は `Quic` / `WebTransport` の 2 つで、`src/session/core.rs` の `validate_setup_role_transport` が WebTransport のときだけ AUTHORITY / PATH を拒否する。
- qmux-rs は最新の develop が `be936f3` (2026-10-14) でタグが 0 件、`shiguredo_qmux` は crates.io に無く (crates.io API が 404 を返す)、`tokio-qmux` は `publish = false` (version 0.0.0) である。依存は git の rev 固定で入れるしかない。
- `tokio_qmux::Connection` は Clone できず、`accept_bidirectional` / `accept_unidirectional` / `recv_datagram` が `&mut self` を要求する。`Config` と `Role` は再エクスポートされておらず、`shiguredo_qmux::config::Config` と `shiguredo_qmux::codec::role::Role` を直接指す必要がある。
- `Config::default()` はフロー制御のクレジットがすべて 0 で `max_datagram_frame_size` が `None` (DATAGRAM 無効) であり、`Connection::new` は不正な `Config` で panic する。
- tokio-qmux のドライバは受信した STOP_SENDING を握りつぶし、アプリへ通知する API を持たない (`shiguredo_qmux` の Sans I/O 層は `Event::StreamStopSending` を通知する)。moqt-rs の QUIC 経路は s2n-quic の `ConnectionObserver` で `Frame::StopSending` を観測し、送信ストリームの終端として MOQT 層へ渡している。
- 仕様面では draft-ietf-moq-transport-22 §6.2 が「TCP など他のトランスポート上で直接動かす方法は定義しない」と明記し、QMux に触れない。QMux 上の MOQT を扱うのは `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` (individual draft、MOQT draft-16 頃を前提とし、QMux の参照も古い) である。
- ALPN は両者で食い違う。draft-ietf-quic-qmux-02 §8.1 は「QMux へのマッピングの識別子は同一プロトコルの QUIC マッピングと異なるものでなければならない (SVCB との互換のため)」と MUST で定め、例として `myapp-12qx` を挙げる。individual draft §3.2 は「native MOQT over QUIC と同じ識別子を使い、新しい ALPN を定義しない」とする。
- ストリームの対応も食い違う。individual draft §4.2 は「control stream は QMux の双方向 stream 0 を MUST で使う」とするが、draft-ietf-moq-transport-22 §6.4.1 の control stream (SETUP、stream type 0x2F00) は単方向ストリームであり、`MoqtClient::establish_quic` も単方向ストリームを 1 本ずつ開いている。
- QMux の DATAGRAM は TCP 上で多重化されるため信頼・順序どおりに届く (draft-ietf-quic-qmux-02 §1.1)。moq-pub は既定で音声を datagram で送るため、この経路では音声が再送される (draft-ietf-moq-transport-22 §6.2 が WebTransport over HTTP/2 について述べる性質と同じ)。
- `tokio_qmux::Connection::recv_datagram` は 1 件ずつ返し、受信は容量 64 件 / 合計 1 MiB の上限を超えると破棄される。moq-sub の datagram 受信タスクは `StreamHandle::recv_datagrams` が「取り出し済みの datagram を await の間に失わない」契約に依存している。

## 設計方針

- 依存は `examples/tokio-moq/Cargo.toml` に `shiguredo_qmux` と `tokio-qmux` を同じ rev の git 依存として追加する (タグが無いためバージョン指定ができない)。本体 `shiguredo_moqt` (no_std、MSRV 1.93) には入れない。qmux-rs の workspace は rust-version 1.95 だが、examples は s2n-quic の都合で既に 1.95 であるため変わらない。
- `examples/tokio-moq/src/qmux.rs` を追加する。`tokio::net::TcpStream::connect` で TCP を張り、`tokio_rustls::TlsConnector` で TLS を確立してから、その TLS ストリームを `tokio_qmux::Connection::new(Role::Client, io, config)` に渡す。TLS の組み立てと `--cert` の扱い (未指定時は検証スキップと警告) は `webtransport_h2.rs` の `build_tls_client_config` に揃える。
- ALPN は draft-ietf-quic-qmux-02 §8.1 の MUST に従い、QUIC の `MOQT_PROTOCOL` (`moqt-22`) とは別の識別子を `qmux.rs` の定数に置く。MOQT 側に登録された値が無いため、同節の `myapp-12qx` の例に倣った `moqt-22qx` を暫定値とする。individual draft §3.2 と食い違うこと、値が暫定であることをコメントと PR 本文に残す。
- `tokio_qmux::Config` は既定値のままでは使えないため明示する。値は `quic.rs` の `build_limits` が決めている同時実行数と窓 (単方向 stream 10,000 / 双方向 stream 1,000 / 接続 64 MiB / stream 16 MiB) に相当する値を写し、`max_datagram_frame_size` を音声の Object が収まる値にする。individual draft §3.3 の推奨値 (100 stream / 1 MiB) は moq-sub の要求に対して小さく、そのままは使わない。値の根拠をコメントに残す。
- `Connection` を 3 つの enum で共有できないため、`qmux.rs` に接続を所有する driver タスクを 1 つ置き、`StreamHandle::Qmux` / `StreamAcceptor::Qmux` / `BidiStreamAcceptor::Qmux` はチャネル越しに open / accept / datagram / close を依頼する。
- driver は `accept_unidirectional` / `accept_bidirectional` を回して receiver へ流し、`closed()` を select して接続終了で待ち続けない (WebTransport 経路の `wait_until_terminated` と同じ役割)。
- datagram は driver が `recv_datagram` を回して蓄積し、`recv_datagrams` は取り出し済みを await の間に失わない契約を維持する。
- `Transport` に `Qmux` を足し、`--transport qmux` で選べるようにする。MSF fragment の `connection` は `q` / `wt` しか定義しないため、`connection=q` と `--transport qmux` は矛盾としてエラーにする (既存の解決規則に合わせる)。
- `transport.rs` の 5 つの enum に QMux を足す。`StreamId` は `as_u64` で `u64` に落とし、`read` の `Ok(None)` を `RequestStreamEnd::Fin`、`Error::Reset` を `RequestStreamEnd::Reset { error_code: Some(code), reliable_size: None }` に対応させる (QMux は RESET_STREAM_AT を実装しないため reliable_size は常に `None`)。
- STOP_SENDING / RESET_STREAM のコード空間は QUIC version 1 のものを多重化するため remap せず、`StopSendingTransport::Qmux` を追加して `Quic` と同じ扱いにする。
- ストリームの対応は QMux が QUIC version 1 のストリームを多重化する性質に従い 1 対 1 にする。control stream は単方向 (stream type 0x2F00)、request stream は双方向とする。individual draft §4.2 の「双方向 stream 0」は draft-ietf-moq-transport-22 §6.4.1 と矛盾するため採らない。採らない理由をコメントに残す。
- 本体ライブラリは `session::types::Transport` に `Qmux` を足すだけにする。`validate_setup_role_transport` は WebTransport だけを拒否の対象にしているため、QMux は native QUIC と同じく AUTHORITY / PATH を載せる。draft-ietf-moq-transport-22 §9.1.1 / §9.1.2 が AUTHORITY / PATH を native QUIC に限るため、この扱いが仕様の範囲外であることをコメントに残す。
- moq-pub / moq-sub の pipeline は、WebTransport 経路と同じく QMux 経路にも experimental である旨の警告ログ (英語) を出す。
- tokio-qmux がピアの STOP_SENDING を通知しない制約は、この経路では送信ストリームの終端を `write` の失敗でしか検出できないことを意味する。解消するには qmux-rs 側に観測 API を足す必要があるため、本 issue では制約として実装コメントに残し、qmux-rs 側の対応が要ることを PR 本文に書く。
- テストはモックを使わず、127.0.0.1 の TCP + rustls (rcgen の自己署名証明書) で QMux のサーバー側を立て、`Session::new_server(Transport::Qmux, ...)` を駆動して確かめる (`webtransport_h2.rs` のテストが TLS サーバーを立てているのと同じ方法)。`examples/tokio-moq/src/peer_harness.rs` は QUIC 専用であるため、QMux 用のハーネスを追加するかは実装時に決める。
- 実 relay の E2E (`e2e-tests/`) は QMux に対応した relay が存在しないため対象外とし、`MOQT_E2E_TRANSPORT` に `qmux` は足さない。
- `examples/tokio-moq/README.md` のトランスポート表と「3 経路」の記述、`examples/moq-pub/README.md` / `examples/moq-sub/README.md` の `--transport` の行、`examples/moq-pub/src/main.rs` / `examples/moq-sub/src/main.rs` の doc コメントを追随させる。
- PR はタイトル先頭に `[WIP]` を付けて作成し、qmux-rs にタグが付くまでマージしない。issue はマージまで open のままにする。

## 完了条件

- `--transport qmux` で moq-pub / moq-sub が QMux 経由で接続し、SETUP と PUBLISH / SUBSCRIBE が成立すること (TCP + TLS + QMux のテスト peer で確認する)。
- 映像 (subgroup stream) と音声 (datagram) の双方が QMux 経由で届くこと。
- `Transport::parse` / `as_str` の逆写像、`resolve_transport` の `connection=q` との矛盾の検出、`StopSendingTransport::Qmux` が remap しないことがテストで固定されていること。
- README と doc コメントが 4 経路に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- PR が `[WIP]` 付きで作成され、依存する qmux-rs の rev、タグが付いたら依存を切り替えること、QMux 経路に残る制約 (STOP_SENDING の観測、datagram が信頼配送になること) が PR 本文に書かれていること。
