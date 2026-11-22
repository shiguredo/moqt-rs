# peer 起点のセッション終了をコードと理由付きで通知できるようにする

- Created: 2026-10-09
- Completed: 2026-11-22
- Branch: feature/add-peer-session-close
- Polished: 2026-10-25
- Refreshed: 2026-11-04

## 目的

peer が transport 層でセッションを終了したとき、その終了コードと、transport が取得できる場合は理由を
アプリが観測できるようにする。

draft-ietf-moq-transport-23 §6.6 (Termination) は、セッションの終了を transport 層で行い、native QUIC では
CONNECTION_CLOSE、WebTransport では CLOSE_WEBTRANSPORT_SESSION カプセルで終了すると定める。
終了コードは §12.2 (Session Termination Codes) の値である (例: CONTROL_MESSAGE_TIMEOUT (0x11))。
現状の Session にはこの終了コードと理由を渡す口が無く、peer が理由付きで終了しても
アプリには常に CLOSED_CONTROL_STREAM (0x306) + 固定文言 ("peer control stream reset" /
"peer control stream closed with FIN") として届く。peer が送ったコードと理由は失われる。

区別したい状況は次の 2 つである。

- transport の終了に伴う control stream の閉鎖 (peer 起点のセッション終了であり、§6.3 違反ではない)
- session の lifetime 中に control stream が不正に閉じられた場合 (§6.3 (Session initialization) 違反)

## 現状

- `src/session/core.rs` の `Session::recv_control_stream_closed(RequestStreamEnd)` は理由 (reason phrase) を受け取る口を持たず、
  `RequestStreamEnd::Reset { error_code }` が運ぶ §12.5 の stream reset コードも `Reset { .. }` として捨てる。
  `RequestStreamEnd::Fin` で `SESSION_CLOSED_CONTROL_STREAM` (0x306) + "peer control stream closed with FIN"、
  `RequestStreamEnd::Reset` で `SESSION_CLOSED_CONTROL_STREAM` (0x306) + "peer control stream reset" を生成する。
- Session のセッション制御系の入力 API は `recv_control` / `recv_control_stream_type` /
  `recv_control_stream_closed` / `recv_request` / `recv_request_stream_closed` / `recv_stream_message` のみで、
  peer 起点のセッション終了をコードと理由付きで渡す口が無い (data plane の `recv_*` は対象外)。
- `SessionEvent::CloseSession` は `SessionError` を運ぶが、transport 終了に伴う peer 起点のセッション終了と、
  ローカル起点 (`Session::close` / `fail`) のセッション終了を区別する情報を持たない
  (`recv_control_stream_closed` は §6.3 違反としてローカル起点で閉じる経路であり、peer の transport 終了を
  コードと理由付きで伝える経路は無い)。
- `SessionError::reason` は `&'static str` である。peer から渡される理由は transport 由来の動的な文字列で
  あり、そのままでは保持できない。
- I/O 層の例: `crates/tokio-moq/src/moqt_client.rs` は control stream の終端を
  `StreamRead::Closed(end)` で受け取り `recv_control_stream_closed(end)` を呼ぶだけで、
  transport が運んだ終了コードと理由を Session へ渡していない。
- moqt-py 経由の実測 (draft-22 時点) では、WT-H3 で server が CONTROL_MESSAGE_TIMEOUT (0x11) と理由付きで終了しても、
  client の close イベントは (3, "peer control stream reset") になった。draft-23 対応後も固定文言のままで、
  コードは `CLOSED_CONTROL_STREAM` (0x306) になる。

## 設計方針

- peer 起点のセッション終了をコードと理由付きで通知する API (`Session::recv_session_closed` 相当) を追加する。
- セッション終了イベントで peer 起点とローカル起点を区別できるようにする。公開 API の変更になるため、
  既存の `SessionEvent::CloseSession` に起点を表すフィールドを追加する案と、peer 起点専用の variant を
  追加する案を比較して決める。どちらの案でも、peer 起点の終了コードと理由をアプリが観測できること。
- `SessionError::reason` を peer の動的な理由を保持できる型 (例: `Cow<'static, str>`) に変える。
  これは `SessionError` の公開フィールドの型変更になるため、`SessionEvent::CloseSession` とあわせて
  公開 API の破壊的変更として扱う。peer の理由の長さと UTF-8 の扱い (transport 依存の値として上限を
  設けないか、§8.5 (Reason Phrase Structure) の 1024 バイト上限を流用するか) も決める。
- 新 API の呼び出し後は `Closing` → (`poll_event` でセッション終了イベント (`CloseSession` または追加する
  専用 variant) を取り出した時点で) `Closed` へ遷移し、後続の `recv_control_stream_closed` を既存の
  Closing / Closed ガードと同じく no-op にする。
- `recv_control_stream_closed` の既存の挙動 (コード・理由・セッション状態) は変えない。transport の終了に
  伴う閉鎖と session 中の不正な閉鎖の区別は I/O 層が行い、transport の終了コードを取得できた場合に
  新 API を呼ぶ (理由は取得できた場合だけ渡し、取得できない場合は空文字列にする)。
- `crates/tokio-moq` の I/O 層は transport の終了コードと理由を取得して新 API を呼ぶ。WebTransport 経路は
  取得できることが確定している (`crates/tokio-moq/src/webtransport_h3.rs` の `WebTransportEvent::SessionClosed` が
  `close_error_code` / `close_message` を、`webtransport_h2.rs` の `WtEvent::SessionClosed` が `error_code` / `reason` を
  運ぶ。h3 は現状ログに出すだけで破棄している)。h3 の `close_error_code` は WT_CLOSE_SESSION カプセルの
  Application Error Code (WebTransport のアプリケーションエラーコード) であり、§12.2 のコードとしてそのまま使う。
  `error_code` は WT_SESSION_GONE などの HTTP/3 プロトコルコードであり、セッション終了コードではない。
  既存の `wt_to_moqt_code` は RESET_STREAM / STOP_SENDING の HTTP/3 コード空間の値を戻すためのものであり、
  WT_CLOSE_SESSION の Application Error Code には適用しない。
- native QUIC (s2n-quic) の `connection::Error::Application` は終了コードを運ぶが、CONNECTION_CLOSE の
  reason phrase は `impl From<ConnectionClose<'_>> for Error` で捨てられるため、公開 API から理由は取得できない。
  このため native QUIC ではコードのみを新 API に渡し、理由は空文字列にする。理由も必要な場合は
  `Subscriber::on_frame_received` で `Frame::ConnectionClose` を観測する追加実装が要る。現在の QUIC 経路の
  `crates/tokio-moq/src/transport.rs` の `StreamAcceptor::accept_recv_stream` は `Err(e)` を
  `TransportError::Quic(format!("{e}"))` に畳むため、コードを渡すには
  `s2n_quic::connection::Error::Application` を判別して `TransportError` にコードを保持する変更も要る。

## 完了条件

- peer 起点のセッション終了コードと理由がセッション終了イベント (既存の `SessionEvent::CloseSession` または
  追加する専用 variant) で観測できること。理由は transport が取得できた場合に限り、取得できない transport
  (native QUIC など) では空文字列になること。
- ローカル起点 (`close` / `fail`) と peer 起点が区別できること。
- 既存の `recv_control_stream_closed` の挙動 (固定コード・固定理由・セッション状態) が変わらないこと。
- 新 API の呼び出し後に届く control stream の終了通知が no-op であること。

## 解決方法

peer 起点のセッション終了を終了コードと理由付きで通知する API を追加し、イベントの起点を
variant で区別した。`recv_control_stream_closed` は従来どおりローカル起点の §6.3 違反として
扱い、コード・理由・状態遷移を変えていない。

### 公開 API の変更

- `Session::recv_session_closed(code, reason)` を追加した。`code` は transport が通知した値を
  そのまま保持し、`reason` は取得できた場合だけ渡す (取得できない場合は空文字列)。
  呼び出し後は `Closing` になり、`poll_event` で `SessionEvent::PeerClosed` を取り出した時点で
  `Closed` になる。`Closing` / `Closed` での呼び出しは no-op であり、後続の
  `recv_control_stream_closed` も no-op になる。
- `SessionEvent::PeerClosed(SessionError)` を追加した。`CloseSession` は自側が決めた終了、
  `PeerClosed` は peer が transport 層で終了した通知である。
- `SessionError.reason` を `&'static str` から `Cow<'static, str>` に変更した。
  `SessionError::new` は `impl Into<Cow<'static, str>>` を受け取り、動的な理由も保持できる。
  `Session::close` / `MoqtClient::close` の `reason` も同様に動的な文字列を受け付ける。
- 理由の長さと UTF-8 の扱いを次のように決めた。`Cow<'static, str>` により UTF-8 であることは
  型で保証する。長さの上限は設けない (transport が運ぶ値であり、MOQT §8.5 の Reason Phrase の
  1024 バイト上限は適用しない)。WebTransport の `WT_CLOSE_SESSION` は transport 側の上限
  (1024 バイト) で切り詰められた値をそのまま保持し、native QUIC では理由を取得できないため
  空文字列にする。

### 設計の比較と選択

`SessionEvent::CloseSession` に起点を表すフィールドを追加する案と、peer 起点専用の variant を
追加する案を比較し、後者を選んだ。

- `CloseSession` は I/O 層が `error.code` / `error.reason` を使って transport へ終了コードを
  送るイベントである。peer 起点の終了は transport が既に閉じているため送り返してはならない。
  起点フィールドを追加する案では、既存のすべてのハンドラが起点を判定する必要があり、
  peer の終了コードを自側の終了コードとして送り返す誤りを型で防げない。
- 専用 variant にすると、peer 起点の終了を扱わない I/O 層・アプリが variant の網羅性検査で
  コンパイルエラーになり、追随漏れを検出できる。
- `SessionError` は本来「本ライブラリが決めた終了のコードと理由」を表す。peer から届く任意の
  transport コードと理由を同じイベントに混ぜず、`PeerClosed` 専用の payload として保持する。
- 一方でセッション終了イベントが 2 種類になるため、`poll_event` の `Closing` → `Closed` 遷移、
  I/O 層の配送対象判定 (`is_notable_event`)、moq-pub / moq-sub のイベント処理を両方に対応させ、
  doc でも両者が終了イベントであることを明記した。

### I/O 層 (`crates/tokio-moq`)

- `TransportError::PeerSessionClosed { error_code }` を追加し、native QUIC のストリーム受信 /
  受理 API が返す peer の `CONNECTION_CLOSE` (application error) のコードを保持する。
  `StreamHandle::peer_session_close` は transport が観測した終了コードと理由を返す。
- QUIC 直接接続はコードのみを渡し、理由は空文字列にした。
  - s2n-quic は `connection::Error::Application` の `initiator` が `Remote` の場合に
    `QuicError::PeerClosed` へ写し、ストリーム受信・送信・受理の各経路でコードを失わない。
  - tobi-quic は `ConnectionError::Application` を `QuicError::PeerClosed` へ写す。起点を公開
    しない API のため、自側 close のコードは `Session` が Closing / Closed であることで
    I/O 層が除外する。
  - ngtcp2 はストリーム API がコードを公開しないため、ドライバが記録した接続終了理由から
    application error code を取り出す。
- WebTransport は `WT_CLOSE_SESSION` の終了コードと理由を保持する。
  - h3 は `WebTransportEvent::SessionClosed` の `close_error_code` / `close_message` を
    `WtSession` と共有し、`error_code` (WT_SESSION_GONE などの HTTP/3 コード) は使わない。
  - h2 は `WtEvent::SessionClosed` の `error_code` / `reason` を保持する。
  - 自側の close に伴う `SessionClosed` は起点として記録しないよう状態で防御する。
- `MoqtClient` は制御ストリームの終端 / 受信エラーのほか、WebTransport のセッション状態の
  変化を直接待ち、peer 起点の終了を `recv_session_closed` で Session に通知する。
  `WT_CLOSE_SESSION` の処理と制御ストリームの `RESET_STREAM` の到着順に依存せず、終了コードを
  失わないためである。制御ストリームが transport 終了を伴わずに閉じた場合は、従来どおり
  `recv_control_stream_closed` で `CLOSED_CONTROL_STREAM` を通知する。
- `CloseSession` は従来どおり transport へ終了コードを送り、`PeerClosed` では送り返さない。

### テスト

- `tests/test_session/session_close.rs`: コードと理由の観測、peer 起点とローカル起点の区別、
  `recv_session_closed` 後の `recv_control_stream_closed` の no-op、1024 バイトを超える UTF-8 の
  理由が切り詰められないことを固定した。
- `crates/tokio-moq/src/peer_harness.rs`: 実 QUIC 接続で peer が `CONNECTION_CLOSE` を送った
  とき、client が `PeerClosed` を終了コード付きで観測し、理由が空文字列になることを固定した。
- `crates/tokio-moq/src/webtransport_h3.rs`: `SessionClosed` の `close_error_code` /
  `close_message` が記録されること、自側 close では記録しないことを固定した。

### 確認

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` /
`prek run --files <変更した全ファイル>` が通ることを確認した。
