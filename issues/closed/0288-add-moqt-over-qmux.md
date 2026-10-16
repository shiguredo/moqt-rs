# examples の moq-pub / moq-sub に QMux 経由の接続を追加する

- Created: 2026-10-16
- Completed: 2026-10-16
- Branch: feature/add-moqt-over-qmux
- Polished: 2026-10-16
- Updated: 2026-10-16

## 目的

UDP が使えない環境でも MOQT を試せるように、順序保証された双方向バイトストリーム (TLS over TCP) の上で QUIC version 1 のストリーム操作と DATAGRAM 操作を提供する QMux (`shiguredo/qmux-rs`) を、examples の接続経路として追加する。`--transport qmux` で moq-pub / moq-sub が QMux 経由で publish / subscribe できる状態にする。

QMux の実装はまだタグが無く、当面タグを打つ予定も無い。準拠する draft-ietf-quic-qmux-02 (2026-07-06、provisional なコードポイントを含む) も、QMux 上の MOQT を扱う `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` (rev 00、2026-02-28 発行、Expires 2026-09-01 で既に失効) も落ち着いていない。このため qmux-rs へは git の rev 固定で依存し、PR は `[WIP]` のままにして、マージの判断はユーザーが行う。

実装は `feature/add-moqt-over-qmux` ブランチで行う (CODEBASE.md の「Pull-Request やブランチを作らず develop にコミットしていくこと」に反するが、本 issue はユーザーの指示による例外とする)。

## 現状

- examples の接続経路は `examples/tokio-moq/src/lib.rs` の `Transport` が `Quic` / `WtH3` / `WtH2` の 3 つで、`Transport::parse` / `as_str` / `resolve_transport` が `--transport` と `--url` の `msf` fragment の `connection` (`q` / `wt`) から解決する。既定は `quic`。
- 接続確立は `examples/tokio-moq/src/lib.rs` の `connect` が `quic` / `webtransport_h3` / `webtransport_h2` の各モジュールへ分岐し、`examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::establish_quic` / `establish_wt` / `establish_wt_h2` が SETUP までを終える。
- 経路ごとの差は `examples/tokio-moq/src/transport.rs` の `SendStream` / `RecvStream` / `StreamHandle` / `StreamAcceptor` / `BidiStreamAcceptor` が吸収する。
- 本体 `shiguredo_moqt::session::types::Transport` は `Quic` / `WebTransport` の 2 つで、`src/session/core.rs` の `validate_setup_role_transport` が WebTransport のときだけ AUTHORITY / PATH を拒否する。
- qmux-rs の develop は `c53b862` (2026-10-16)。`be936f3` (2026-10-14) から `520bced` (ピアの STOP_SENDING 通知 API)、`7f3e291` (accept 系と DATAGRAM 受信の `&self` 化)、`ffca089` (接続単位の STOP_SENDING 観測 API) が入った。
- `c53b862` で `Connection::new` が `Result` になり、`recv_datagrams` / `peer_transport_parameters` / `dropped_datagrams` が追加され、README と doc (ALPN、cancel safety、STOP_SENDING) が整備された。
- qmux-rs はタグが 0 件で、`shiguredo_qmux` は crates.io に無く (crates.io API が 404 を返す)、`tokio-qmux` は `publish = false` (version 0.0.0) である。依存は git の rev 固定で入れるしかない。
- `tokio_qmux::Connection` は Clone ではないが、`open_*` / `accept_*` / `send_datagram` / `recv_datagram` / `recv_datagrams` / `recv_stop_sending` / `close` / `closed` がすべて `&self` であるため、`Arc<Connection>` を複数タスクで共有して `select!` で待てる (accept 系と DATAGRAM の receiver は内部の `tokio::sync::Mutex` で待ちを直列化する)。
- tokio-qmux の crate 直下の再エクスポートは `Connection` / `Error` / `Result` / `RecvStream` / `SendStream` / `StopSendingObserved` である。`Config` は `shiguredo_qmux::config::Config`、`Role` は `shiguredo_qmux::codec::role::Role` を直接指す必要がある。
- `Config::default()` はフロー制御のクレジットがすべて 0 で `max_datagram_frame_size` が `None` (DATAGRAM 無効) である。`Connection::new` は送信できない設定 (`max_record_size` が既定値未満など) で `Err(tokio_qmux::Error::Qmux(..))` を返すため、ストリームと DATAGRAM を使うには明示的な設定が必要である。
- `recv_datagrams` (1 件以上届くまで待ち、到着済みをまとめて返す。返る `Vec` は空にならない)、`peer_transport_parameters`、`dropped_datagrams` (破棄した受信 DATAGRAM の累計) が追加された。`accept_*` / `recv_datagram` / `recv_datagrams` / `recv_stop_sending` は doc に cancel safe と明記されている。
- STOP_SENDING は tokio-qmux がストリーム単位 (`SendStream::stopped`) と接続単位 (`Connection::recv_stop_sending` が返す `StopSendingObserved { id, error_code }`) の両方へ通知する。`write` / `finish` / `reset` は通知済みなら `Error::Stopped` を返す。
- moqt-rs の QUIC 経路は s2n-quic の `ConnectionObserver` で `Frame::StopSending` を観測し、接続単位の `StopSendingObserved` を MOQT 層へ渡している。
- 仕様面では draft-ietf-moq-transport-22 §6.2 が「TCP など他のトランスポート上で直接動かす方法は定義しない」と明記し、QMux に触れない。QMux 上の MOQT を扱う individual draft は §10.1 で MOQT draft-16 (2026-01) と draft-opik-quic-qmux-00 (2025) を参照しており、QMux の参照は 2 世代古い。
- control stream の対応は draft-ietf-moq-transport-22 §6.4.1 の単方向ストリーム (stream type 0x2F00) が現行で、individual draft §4.2 の「双方向 stream 0 を MUST」とは矛盾する。
- ALPN は両者で食い違う。draft-ietf-quic-qmux-02 §8.1 は「QMux へのマッピングの識別子は同一プロトコルの QUIC マッピングと異なるものでなければならない (SVCB との互換のため)」と MUST で定める。individual draft §3.2 は「native MOQT over QUIC と同じ識別子を使い、新しい ALPN を定義しない」とする。MOQT 側に QMux 用の登録は無い (draft-ietf-moq-transport-22 §16 の ALPN registry は未整備)。
- QMux の DATAGRAM は TCP 上で多重化されるため信頼・順序どおりに届く (draft-ietf-quic-qmux-02 §1.1)。moq-pub は既定で音声を datagram で送る (`--audio-subgroup` の否定で `audio_datagram` が真) ため、この経路では音声が再送される (draft-ietf-moq-transport-22 §6.2 が WebTransport over HTTP/2 について述べる性質と同じ)。
- `tokio_qmux::Connection::recv_datagram` は 1 件ずつ、`recv_datagrams` は到着済みをまとめて返す。受信は容量 64 件 / 合計 1 MiB の上限を超えると破棄され、破棄数は `dropped_datagrams` で観測できる。
- moq-sub の datagram 受信タスクは、`StreamHandle::recv_datagrams` が「取り出し済みの datagram は失われない」性質に依存している (`examples/moq-sub/src/pipeline.rs` のコメント)。この性質は `examples/tokio-moq/src/transport.rs` の doc には書かれておらず、実装と pipeline 側のコメントに依存している。
- QMux の仕様 (draft-ietf-quic-qmux-02) の一次資料が `refs/quic/` に無い。`refs/` で QMux を含むのは `refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` だけである。

## 設計方針

- `refs/quic/draft-ietf-quic-qmux-02.txt` を追加する。QMux の節番号を引用する根拠をリポジトリ内に置くためである (同ファイルは qmux-rs の `refs/quic/` にある)。
- 依存は `examples/tokio-moq/Cargo.toml` に `shiguredo_qmux` と `tokio-qmux` を同じ rev の git 依存 (`git = "https://github.com/shiguredo/qmux-rs"` と `rev`) として追加する。
- qmux-rs は当面タグを打つ予定が無く、`shiguredo_qmux` は crates.io に無く、`tokio-qmux` は `publish = false` であるため、これが唯一の取り込み方である。`shiguredo-rust` の「バージョン番号はマイナーバージョンまで指定すること」の例外である理由を Cargo.toml のコメントに残す。
- 本体 `shiguredo_moqt` (no_std、MSRV 1.93) には入れない。qmux-rs の workspace は rust-version 1.95 だが、examples は s2n-quic の都合で既に 1.95 であるため変わらない。
- qmux-rs の rev は PR 本文に記録する。上げるときは同じブランチで rev、`Cargo.lock`、追随が必要な API 変更をまとめて更新する。
- `examples/tokio-moq/src/qmux.rs` を追加する。`tokio::net::TcpStream::connect` で TCP を張り、`tokio_rustls::TlsConnector` で TLS を確立してから、その TLS ストリームを `tokio_qmux::Connection::new(Role::Client, io, config)` に渡す。
- `Connection::new` は `Result` を返すため `?` で伝播する。接続の組み立ては qmux-rs の `crates/tokio-qmux/examples/basic.rs` も参考にする。
- QMux 固有の処理 (TCP + TLS の接続、`Config` の組み立て、accept を流すタスク、datagram の取得、STOP_SENDING の詰め替え、ピアの `max_datagram_frame_size` の確認) は `examples/tokio-moq/src/qmux.rs` に置く。qmux-rs の API が変わったときに追随する範囲を局所にする。
- 共有側への追加は、`transport.rs` の 5 つの enum と `StopSendingTransport`、`error.rs` の `TransportError`、`lib.rs` の `Transport` と `connect`、本体 `session::types::Transport`、`MoqtClient::establish_qmux` (`establish_quic` と同じ結線の新しい関数) に限る。
- TLS の組み立ては `webtransport_h2.rs` の `build_tls_client_config`、`--cert` 未指定時の警告は `lib.rs` の `apply_tls_config_h2` に揃える。
- ALPN は individual draft §3.2 に従い、native QUIC と同じ `MOQT_PROTOCOL` (`moqt-22`) を使う。QMux 上の MOQT 専用の識別子は定義せず、下位トランスポートは接続そのもので区別する。
- draft-ietf-quic-qmux-02 §8.1 は「QMux へのマッピングの識別子は同一プロトコルの QUIC マッピングと異なるものでなければならない (SVCB との互換のため)」と MUST で定めるため、この選択は同節から外れる。draft-ietf-moq-transport-22 §6.1.2 は SVCB 互換レコードでエンドポイントと対応 ALPN を学ぶ MAY を置き、TCP+TLS では h2 で WebTransport を確立するとするが、レコードを定義する draft は未整備 (TODO) である。
- MOQT 側に QMux 用の ALPN 登録が無いため interop を優先して `moqt-22` を使う。qmux-rs の `docs/INTEROP.md` も「MOQT over QMux: `moqt-22`」と同じ整理を記録している。SVCB 上で QUIC 用と区別できない衝突可能性と、両 draft が決着したら見直すことをコメントと PR 本文に残す。
- `tokio_qmux::Config` は既定値のままでは使えないため明示する。値は `quic.rs` の `build_limits` が決めている同時実行数と窓 (単方向 stream 10,000 / 双方向 stream 1,000 / 接続 64 MiB / stream 16 MiB) に相当する値を写す。
- `max_datagram_frame_size` は `max_record_size` の既定 16382 に合わせる。qmux-rs の `Connection::send_datagram` は DATAGRAM フレーム全体 (Type 1 バイト + Length + payload。Length は payload が 64 バイト以上なら 2 バイト) をピアの `max_datagram_frame_size` と `max_record_size` の両方と比較する。
- `--datagram-max-size` の既定 1160 の OBJECT_DATAGRAM を送るには 1163 以上が必要で、16382 なら既定の Record に収まる DATAGRAM を受け入れられる。値の根拠をコメントに残す。
- individual draft §3.3 の推奨値 (接続 1 MiB、per-stream 256 KiB、stream 数 100、max_frame_size 16384) は moq-sub の要求に対して小さく、そのままは使わない。値の根拠をコメントに残す。
- `Arc<tokio_qmux::Connection>` を `StreamHandle::Qmux` / `StreamAcceptor::Qmux` / `BidiStreamAcceptor::Qmux` が共有する。`open_*` / `send_datagram` / `close` は直接呼び、`accept_unidirectional` / `accept_bidirectional` は receiver へ流すタスクから待ち、`closed()` を select して接続終了で待ち続けない (`wait_until_terminated` と同じ役割)。
- datagram は `StreamHandle::Qmux` が `Connection::recv_datagrams` をそのまま呼ぶ。1 件以上届くまで待って到着済みをまとめて返し、cancel safe であるため、`StreamHandle::recv_datagrams` の契約 (取り出し済みの datagram は失われない) を自前のバッファやタスクなしで満たせる。`dropped_datagrams` をログに残し、上限による取りこぼしを追えるようにする。
- 接続確立後 (`establish_qmux` が制御ストリームを開いた後) に `peer_transport_parameters` でピアの `max_datagram_frame_size` を確認する。tokio-qmux は `send_datagram` の失敗を呼び出し元へ返さないため、音声が無言で落ちるのを防ぐ。
- ピアの値が `None` (DATAGRAM 非対応) か、`--datagram-max-size` の payload に対する DATAGRAM フレーム全体の大きさ (既定 1160 なら 1163) に満たない場合は警告ログ (英語) を出す。
- STOP_SENDING は `Connection::recv_stop_sending` を待つタスクが `crate::quic::StopSendingObserved { stream_id: id.as_u64(), error_code }` に詰め替え、QUIC 経路と同じ `StopSendingReceiver` のチャネルへ流す。`MoqtClient::handle_stop_sending` と `establish_quic` と同じ結線を `establish_qmux` でも行う。`SendStream::stopped` は送信と同時に await できないため使わない。
- `Transport` に `Qmux` を足し、`--transport qmux` で選べるようにする。MSF fragment の `connection` は `q` / `wt` しか定義しないため、`connection=q` と `--transport qmux` は矛盾としてエラーにする (`resolve_transport` の既存の腕で自動的に成立することをテストで固定する)。
- `examples/tokio-moq/src/error.rs` の `TransportError` に QMux 用のバリアントを足し、`tokio_qmux::Error` (Qmux / Io / ConnectionClosed / Reset / Stopped / DriverStopped) の写像を決める。既存の `Quic(String)` / `Http3(..)` / `Http2(..)` / `WtH2(..)` と同じ粒度に揃える。
- `transport.rs` の 5 つの enum に QMux を足す。`shiguredo_qmux::stream::StreamId` は `as_u64` で `u64` に落とし、`read` の `Ok(None)` を `RequestStreamEnd::Fin`、`Error::Reset` を `RequestStreamEnd::Reset { error_code: Some(code), reliable_size: None }` に対応させる (QMux は RESET_STREAM_AT を実装しないため reliable_size は常に `None`)。
- STOP_SENDING / RESET_STREAM のコード空間は QUIC version 1 のものを多重化するため remap せず、`StopSendingTransport::Qmux` を追加して `Quic` と同じ扱いにする。
- ストリームの対応は QMux が QUIC version 1 のストリームを多重化する性質に従い 1 対 1 にする。control stream は単方向 (stream type 0x2F00)、request stream は双方向とする。individual draft §4.2 の「双方向 stream 0」は draft-ietf-moq-transport-22 §6.4.1 と矛盾するため採らない。採らない理由をコメントに残す。
- 本体ライブラリは `session::types::Transport` に `Qmux` を足すだけにする。`validate_setup_role_transport` は WebTransport だけを拒否の対象にしているため、QMux は native QUIC と同じく AUTHORITY / PATH を載せる。draft-ietf-moq-transport-22 §9.1.1 / §9.1.2 が AUTHORITY / PATH を native QUIC に限るため、この扱いが仕様の範囲外であることをコメントに残す。
- moq-pub / moq-sub の pipeline は、WebTransport 経路と同じく QMux 経路にも experimental である旨の警告ログ (英語) を出す。理由は WebTransport の `RESET_STREAM_AT` とは異なるため、QMux 用の理由 (仕様も実装も流動的であること、datagram が信頼配送になること) を書く。
- テストはモックを使わず、127.0.0.1 の TCP + rustls (rcgen の自己署名証明書) で QMux のサーバー側を立て、`Session::new_server(Transport::Qmux, ...)` を駆動して確かめる (`webtransport_h2.rs` のテストが TLS サーバーを立てているのと同じ方法)。
- `examples/tokio-moq/src/peer_harness.rs` は QUIC 専用であるため、QMux 用のハーネスを追加するかは実装時に決める。
- 実 relay の E2E (`e2e-tests/`) は QMux 経路を終端できる MoQ リレーが確認できていないため対象外とし、`MOQT_E2E_TRANSPORT` に `qmux` は足さない。
- 3 経路を列挙している記述を追随させる。対象は `examples/tokio-moq/README.md` のトランスポート表と「3 経路」、リポジトリ直下 `README.md`、`examples/moq-pub/README.md` / `examples/moq-sub/README.md` の `--transport` の行である。
- あわせて `examples/moq-pub/src/cli.rs` / `examples/moq-sub/src/cli.rs` の `.doc(...)`、`examples/tokio-moq/src/lib.rs` の crate ドキュメントと `connect` / `ConnectParams` の doc コメント、`examples/tokio-moq/src/moqt_client.rs` の doc コメントを追随させる。
- `examples/moq-pub/src/main.rs` / `examples/moq-sub/src/main.rs` の doc コメントと `e2e-tests/README.md` の環境変数表も追随させる。
- PR はタイトル先頭に `[WIP]` を付けて作成し、マージの判断はユーザーが行う。`/auto-resolve` などの自動化に載せた場合も、PR 作成後に自動マージせず停止して判断を仰ぐ。
- issue は `shiguredo-issues` の手順どおり、`## 解決方法` の追記と `issues/closed/` への移動を作業ブランチ上で PR のマージ前に行う (`develop` 側の状態はマージで closed になる)。

## 完了条件

- `--transport qmux` で moq-pub / moq-sub が QMux 経由で接続し、SETUP と PUBLISH / SUBSCRIBE が成立すること (TCP + TLS + QMux のテスト peer で確認する)。
- 映像 (subgroup stream) と音声 (datagram) の双方が QMux 経由で届くこと。
- ピアの STOP_SENDING が MOQT 層へ届き、該当する送信ストリームの終端として扱われること (`Connection::recv_stop_sending` を使う)。
- `--datagram-max-size` の既定 1160 の音声 Object が QMux の `max_datagram_frame_size` に収まることがテストで固定されていること (tokio-qmux は `send_datagram` の失敗を呼び出し元へ返さないため、上限が足りないと音声が無言で落ちる)。
- `Transport::parse` / `as_str` の逆写像、`resolve_transport` の `connection=q` との矛盾の検出、`StopSendingTransport::Qmux` が remap しないことがテストで固定されていること。
- 3 経路を列挙している README と doc コメントが 4 経路に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- PR が `[WIP]` 付きで作成され、依存する qmux-rs の rev、rev 固定で取り込む理由、QMux 経路に残る制約 (datagram が信頼配送になること、RESET_STREAM_AT 未実装のため `reliable_size` が常に `None` になること) が PR 本文に書かれていること。

## 解決方法

`--transport qmux` で moq-pub / moq-sub が QMux (draft-ietf-quic-qmux-02) 経由で接続できるようにした。

### 追加

- `examples/tokio-moq/src/qmux.rs`: TCP + TLS (ALPN は native QUIC と同じ `moqt-22`) で `tokio_qmux::Connection` を確立する
  - `Config` は `max_idle_timeout` を 30 秒、フロー制御のクレジットを QUIC 直接接続に相当する量、`max_datagram_frame_size` を `max_record_size` の既定 (16382) に設定する
  - peer 起動ストリームの受理と STOP_SENDING の観測は接続を共有したタスクで待ち、接続が閉じた場合とアプリがアクセプターを drop した場合にタスクを終了させる (タスクが接続を握り続けるとドライバが終了しない)
  - DATAGRAM の上限はピアの `max_datagram_frame_size` と `max_record_size` の両方と突き合わせ、既定 1160 の OBJECT_DATAGRAM (フレーム全体 1163) が収まらない場合に警告ログを出す
  - 破棄された受信 DATAGRAM は `dropped_datagrams` から警告ログに出す
- `examples/tokio-moq/src/tls.rs`: QMux と WebTransport over HTTP/2 が共有する rustls のクライアント設定
- `refs/quic/draft-ietf-quic-qmux-02.txt`

### 変更

- `examples/tokio-moq/Cargo.toml`: qmux-rs の `shiguredo_qmux` と `tokio-qmux` を同じ rev (`c53b862`) の git 依存として追加する (タグが無く crates.io に未公開のため)
- `examples/tokio-moq/src/transport.rs`: `SendStream` / `RecvStream` / `StreamHandle` / `StreamAcceptor` / `BidiStreamAcceptor` と `StopSendingTransport` に QMux を追加する。`RecvStream::stop_sending` は QMux の API に合わせて非同期にする
- `examples/tokio-moq/src/error.rs`: `TransportError::Qmux` を追加し、`Error::Stopped` を `StreamReset`、接続クローズを `ConnectionClosed` に写す
- `examples/tokio-moq/src/lib.rs`: `Transport::Qmux` (`--transport qmux`) と `connect` の QMux の腕、`ConnectParams::datagram_max_size` を追加する
- `examples/tokio-moq/src/moqt_client.rs`: `MoqtClient::establish_qmux` を追加する (`establish_quic` と同じ結線)
- `src/session/types.rs`: 本体の `Transport` に `Qmux` を追加する
- `examples/moq-pub` / `examples/moq-sub`: CLI の `--transport` の説明、experimental の警告、`TransportError` の表示を追随させる
- README・doc コメント・`docs/missing-features.md`・`skills/shiguredo-moqt/SKILL.md` を 4 経路に追随させる

### テスト

- `qmux.rs`: TCP + TLS + QMux のテスト peer (`peer_harness.rs` の `QmuxTestPeerServer`) を使い、SETUP の交換と PUBLISH / SUBSCRIBE の成立、subgroup stream と DATAGRAM (既定の 1160 を含む) の往復、ピアの STOP_SENDING が接続単位の観測と送信ストリームの終端の双方に現れることを検証する。DATAGRAM の上限判定は純関数として両方の上限を検証する
- `lib.rs` / `cli.rs`: `Transport::parse` / `as_str` の逆写像、`connection=q` / `connection=wt` と `--transport qmux` の矛盾検出、`--transport qmux` の解釈
- `transport.rs`: `StopSendingTransport::Qmux` が remap しないこと

### 確認

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --files` / no_std ビルド / aws-lc-rs feature の clippy と rustdoc が通ることを確認した。

### 残る制約

- ALPN は native QUIC と同じ `moqt-22` を使う (draft-ietf-quic-qmux-02 Section 8.1 の MUST からは外れる。individual draft Section 3.2 に従う)
- DATAGRAM は経路での損失を下位の TCP が再送するため起きないが、QMux は速やかに配送できない DATAGRAM の破棄を許すため (Section 6)、受信側のバッファが溢れると欠落しうる
- `RESET_STREAM_AT` 未実装のため `reset` の `reliable_size` は常に `None` になる
- 制御ストリームは peer 起動の最初の単方向ストリームとして扱う (QUIC 直接接続と同じ)
- 「datagram が信頼配送になること」ではなく「経路での損失は下位の TCP が再送するが、QMux は速やかに配送できない DATAGRAM の破棄を許す」が正しい (Section 6)
- qmux-rs のドライバは、自側の `close` の後もピアの shutdown を待つため終了しない (ピアが読み取りを止めると `flush` の `write_all` にも timeout が無い)
  - `Connection` の drop がドライバ終了の引き金であるため、双方向 accept タスクは `MoqtClient` 内の detached な受理タスクが `bi_rx` を保持し続ける限り `tx.closed()` も `connection.closed()` も成立せず、接続・タスク・TCP ソケットが残る
  - 単方向 accept と STOP_SENDING のタスクはアプリ側の receiver の drop で終了する
  - qmux-rs 側でドライバに linger の上限を設けるか、`close` でドライバを終了できるようにするのが本筋である (example 側では解消できない)
