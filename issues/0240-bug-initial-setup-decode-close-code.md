# SETUP 交換の失敗時にセッション終了コードを送る

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-initial-setup-decode-close-code
- Polished: 2026-10-13

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は "If the Token structure
cannot be decoded, the receiver MUST close the Session with KEY_VALUE_FORMATTING_ERROR." と定める。
peer の初回 SETUP の decode に失敗した場合も、SETUP の検証 (role / transport / URI /
REGISTER alias) に失敗した場合も、example は接続を切るだけで終了コードを送らない。確立後の
decode 失敗は 0222 で終了コード付きで閉じるようにしたため、確立前 (SETUP 交換中) も同じ扱いに
そろえる。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `finish_handshake` は
  `control_recv.recv_message().await?` で decode 失敗を `TransportError::Moqt` として返し、
  呼び出し元 (`MoqtClient::establish_quic` / `establish_wt` / `establish_wt_h2` → `moq-pub` /
  `moq-sub` の pipeline) が `?` で終了する。session は CloseSession を発行しない。
- 同じく `finish_handshake` の `session.recv_control(peer_setup)?` は、role / transport / URI /
  REGISTER alias の検証に失敗すると `SessionError` を返す。session は `fail` で `CloseSession` を
  積んでいるが、`?` で return するため queue は session ともに破棄され、終了コード
  (INVALID_PATH (0x8) / INVALID_AUTHORITY (0x19) / PROTOCOL_VIOLATION (0x3)) は peer に
  届かない。`AUTH_TOKEN_CACHE_OVERFLOW` (0x13) は容量超過が `Ok(false)` として無視され、
  `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) は `SetupOptions::decode` の `validate_setup_scope` が
  `ProtocolViolation` で先に落ちるため SETUP 交換中には到達しない。
  `DUPLICATE_AUTH_TOKEN_ALIAS` (0x14) も到達しない。peer の cache は自側 SETUP の
  `MAX_AUTH_TOKEN_CACHE_SIZE` を上限に作られるが、example の自側 SETUP はこの option を
  宣言しないため上限が 0 になり、REGISTER は常に `Ok(false)` で無視される。
  `MALFORMED_PATH` (0x9) / `MALFORMED_AUTHORITY` (0x1A) は server 役の SETUP を検証する経路
  (`validate_setup_role_transport` が先に成立する) では到達しないため、本 issue の対象外とする
  (到達させるには peer を client 役にするハーネスが要る)。
- 確立前の同じ欠陥クラスが他にもある。`establish_*` の `read_stream_type().await?` と
  `recv_control_stream_type(...)?` (stream type 不一致は `PROTOCOL_VIOLATION`)、peer の
  control stream が SETUP 前に終端した場合 (`recv_control_stream_closed` が
  `PROTOCOL_VIOLATION` の `CloseSession` を積むが戻り値を捨てて `TransportError::Internal` を
  返す) である。確立後は `pump_once` が同じ通知の直後に `drain_events` を呼ぶため終了コードが
  届いており、handshake 経路だけが drain しない。
- 終了コードの写像 `session_error_code` は `examples/moq-pub/src/pipeline.rs` と
  `examples/moq-sub/src/pipeline.rs` に private で定義されている (`MessageError` の
  `KeyValueFormattingError` → `SESSION_KEY_VALUE_FORMATTING_ERROR`、`MalformedAuthToken` →
  `SESSION_MALFORMED_AUTH_TOKEN`、それ以外 → `SESSION_PROTOCOL_VIOLATION`)。`finish_handshake` は
  `examples/tokio-moq` 側にあり、失敗時点で `MoqtClient` が存在しないため pipeline から
  閉じることはできない。
- `Session::close` は `fail` を呼び、Closing / Closed 以外では状態を問わず `CloseSession` を
  積む。SETUP 交換中 (state は `LocalSetupSent`) でも終了コードを送れる。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` の
  `session_error_code` の doc は「peer の初回 SETUP の decode 失敗は接続確立
  (`MoqtClient::establish_*`) の時点で確定し、確立前に終了コードを送れないため本関数には
  届かない (既知の限界)」と書いており、本対応で実態が変わる。なお transport 自体のエラーでは
  接続が死んでいるため終了コード付きの close を送れないという別の限界の記述
  (`examples/moq-sub/src/pipeline.rs`) は本 issue の対象外である。

## 設計方針

- `finish_handshake` と `establish_*` の確立前の失敗を、終了コード付きのセッション終了に
  そろえる。失敗の種類ごとに次のように扱う。
  - SETUP の decode 失敗 (`control_recv.recv_message()` の `MessageError`): 写像
    (後述) で得たコードで `Session::close(code, MessageError::reason())` を呼ぶ。コードは
    §8.3 の書式違反 (`KeyValueFormattingError`。`AuthorizationToken::decode` は Alias Type が
    読めない / 未知 / フィールド欠落 / 余剰バイトのいずれもここに落ちる) が
    `KEY_VALUE_FORMATTING_ERROR` (0x6)、同一メッセージ内の (Token Type, Token Value) 重複
    (`MalformedAuthToken`) が `MALFORMED_AUTH_TOKEN` (0x16)、それ以外が
    `PROTOCOL_VIOLATION` (0x3) である。
  - session 層の検証失敗 (`recv_control` / `recv_control_stream_type` /
    `recv_control_stream_closed` の `SessionError`): session は既に Closing で `Session::close` は
    no-op になるため、`poll_event` を `None` になるまで回して `CloseSession` を取り出す
    (1 回だけ呼んで `matches!` で判定しないこと)。取り出せない場合は渡されたコードで
    `Session::close` する。
  - 終了コードを wire に載せるのは `StreamHandle::close(code, reason)` である
    (`examples/tokio-moq/src/transport.rs`。QUIC は CONNECTION_CLOSE になる。
    draft-ietf-moq-transport-22 §6.6 (Termination))。`drain_events` の `CloseSession` 処理と
    同じ経路であり、制御ストリーム (`control_send`) へメッセージを書くのではない。
  - 上記を 1 つのヘルパ (`close_handshake_session` など) にまとめ、`establish_quic` /
    `establish_wt` / `establish_wt_h2` / `finish_handshake` の各失敗経路から呼ぶ。ヘルパは
    「session が `CloseSession` を積んでいればそのコードを、いかなければ渡されたコードを使う」
    形にし、渡されたコードは次のように呼び出し側で決める。
    - decode 失敗と `read_stream_type` の `TransportError::Moqt` の失敗: 呼び出し側で
      `TransportError::Moqt(e)` を variant マッチして `e` を取り出し、`session_error_code(&e)` を
      渡す (`read_stream_type` は decode 失敗を `TransportError::Moqt` で返す)。`close` の reason は
      `MessageError::reason()` を使う。`read_stream_type` の I/O 失敗 (transport 自体の失敗) は
      後述の対象外に含める。
    - `recv_control` / `recv_control_stream_type` / `recv_control_stream_closed` の失敗: session が
      積んだ `CloseSession` の `SessionError` からコードと reason (どちらも取得できる) を使い、
      取り出せない場合のフォールバックだけ渡されたコードと固定文字列にする。
    - `?` で早期 return している箇所は `match` に書き換えて、ヘルパを呼んでから error を
      返す形にする。
  - 本 issue の対象外は次である。自側の失敗 (自側 SETUP の encode 失敗
    (`send_control_stream_setup`)、`Session::new_client` の失敗、`pop_send_control` の失敗) は
    peer の違反ではないため終了コードを送る対象にしない。transport 自体の失敗
    (`open_send_stream` / acceptor の失敗、control stream の受信 I/O 失敗
    (`read_stream_type` / `recv_message` が `TransportError::Moqt` 以外を返す場合)) は接続が
    死んでいるため送れない。
  - 確立後の `pump_once` の `recv_control` / `recv_stream_message` の失敗は、本 issue と同じ
    欠陥クラス (session が `CloseSession` を積んでいるのに `?` が `drain_events` より先に
    return して終了コードが届かない) だが、確立前の SETUP 交換とは別の経路であるため本 issue の
    対象外とし、別 issue で扱う (`recv_request` は `drain_events` を呼んでおり欠陥は無い)。
- 終了コードの写像を `examples/tokio-moq/src/error.rs` へ `pub fn session_error_code(error:
  &MessageError) -> u64` として移設し、`moq-pub` / `moq-sub` の pipeline は tokio-moq の関数を
  使う (`examples/tokio-moq/src/lib.rs` の方針どおり最上位再エクスポートは設けず、
  `tokio_moq::error::session_error_code` を参照する)。pipeline 側の private 定義は削除し、
  移設先で同等の単体テストを維持する (既存の `session_error_code_maps_message_failures` /
  `session_error_code_maps_decode_failures` は移設先のテストとして書き替える)。
- 写像の doc から「初回 SETUP では終了コードを送れない (既知の限界)」を除き、確立前も
  `session_error_code` で終了コードを決めることを書く (`moq-pub` / `moq-sub` の pipeline の doc
  も同様に実態へ合わせる)。
- 確立後の `pump_once` の `recv_control(...).map_err(TransportError::Internal)?` は本 issue の
  対象外とする (SETUP 交換中ではなく、確立後に届いた SETUP の扱いである)。必要になった時点で
  別 issue にする。
- 検証は `examples/tokio-moq/src/peer_harness.rs` (0242) を拡張して行う。追加するのは次の 3 つ
  の能力 (と、その能力で表現する 3 つの経路) である。
  - peer が control stream の先頭に送る SETUP を差し替える経路。`establish` は
    `Session::new_server` が積んだ SETUP を送るだけなので、`SetupOptions` を渡して SETUP を
    組み立てる経路と、control stream の先頭に書く完全なバイト列 (stream type prefix と
    メッセージ本体) をそのまま渡す経路を追加する。後者では prefix も呼び出し側が決めるため、
    誤った stream type prefix を送る経路も同じ経路で表現できる。
  - SETUP を送らずに control stream を終端する経路を追加する (現行の `establish` は SETUP を
    送ったうえ control stream を保持するため、この失敗を再現できない)。
  - peer 側で client の CONNECTION_CLOSE の終了コードを観測する経路。s2n-quic の event
    subscriber (`Server::builder().with_event(...)` の `on_connection_closed`) が受け取る
    `s2n_quic::connection::Error::Application` からコードを取り出し、テストが読めるようにする。
  - テストの入力と期待コードは次を使う。
    - トークン構造の decode 失敗: AUTHORIZATION_TOKEN (0x03) の値に未知の Alias Type (0x04) を
      置いた SETUP。`AuthorizationToken` は未知の Alias Type を表現できず、`SetupOptions::encode`
      も `Bytes` を AUTHORIZATION_TOKEN に置けば拒否するため、`ControlMessage::Setup` では
      構築できない。生バイト列経路に
      `shiguredo_moqt::stream::encode_control_stream_setup(&ControlMessage::Unsupported {
      type_id: shiguredo_moqt::stream::SETUP_STREAM_TYPE, request_id: None, body })` の結果
      (stream type prefix 込み。body は未知 Alias Type を持つ生の Setup Options) を渡す。
      期待コードは `KEY_VALUE_FORMATTING_ERROR` (0x6) である。
    - トークンの重複: 同一 (Token Type, Token Value) の REGISTER トークンを 2 つ載せた SETUP。
      `SetupOptions::encode` は重複を拒否するため、上記の生バイト列経路で送る。期待コードは
      `MALFORMED_AUTH_TOKEN` (0x16) である。
    - session 層の検証失敗: PATH オプションを持つ SETUP (値は `/evil` のような正当な書式でよい)。
      server 役の peer が PATH を送ることが失敗理由になり、期待コードは `INVALID_PATH` (0x8)
      である。ハーネスの session は `Session::new_server(Quic, SetupOptions::new())` のままにし
      (server の PATH は `Session::new` が `INVALID_PATH` で拒否するため渡せない)、送信する
      SETUP だけを差し替える。
    - 確立前の stream type 不一致: SETUP の代わりに誤った stream type prefix を送る。期待コードは
      `PROTOCOL_VIOLATION` (0x3) である。
    - SETUP 前の control stream 終端: SETUP を送らずに control stream を FIN で終端する。
      期待コードは `PROTOCOL_VIOLATION` (0x3) である。
- ハーネスの module doc の「提供しない」から、追加する 2 つの能力 (SETUP の差し替え、
  終了コードの観測) を外す。
- 観測する transport は QUIC 直接接続 (`MoqtClient::establish_quic`) とする。WebTransport
  over HTTP/3 / HTTP/2 の確立前失敗は本 issue の検証対象に含めない (実装は 3 経路とも
  そろえるが、テストは QUIC で行う)。
- ハーネスは `127.0.0.1` の単一アドレスへ接続するため、`connect_with_fallback` の再試行
  (複数アドレス時) はテストに影響しない。

## 完了条件

- peer の初回 SETUP の decode に失敗したとき、example が写像で決まる終了コード (§8.3 の書式
  違反なら `KEY_VALUE_FORMATTING_ERROR` (0x6)、§8.9 の重複トークンなら
  `MALFORMED_AUTH_TOKEN` (0x16)、それ以外は `PROTOCOL_VIOLATION` (0x3)) でセッション終了を
  送ってから終了すること。peer 側で CONNECTION_CLOSE のコードを観測して固定する。
- SETUP 交換中に session 層の検証が失敗したときも、対応する §12.2 のコード (PATH なら
  `INVALID_PATH` (0x8) など) でセッション終了を送ってから終了すること。確立前の stream type 不一致と SETUP 前の control stream 終端も同じ
  扱いになっていること。
- 終了コードの写像が `tokio_moq::error::session_error_code` に集約され、`moq-pub` / `moq-sub` が
  同じ関数を使っていること (呼び出し関係はテストでは固定できないため、コードと単体テストで
  確認する)。
- `tokio_moq::error::session_error_code` の doc から「初回 SETUP では終了コードを送れない」という
  既知の限界の記述が消え、確立前もこの写像でコードを決めることが書かれ、`moq-pub` /
  `moq-sub` の doc も実態に合っていること。
- 上記がテストで固定されていること (確立前の失敗は peer が CONNECTION_CLOSE のコードを
  観測する)。モック / スタブを使わないこと。

## 解決方法

- `examples/tokio-moq/src/moqt_client.rs` に `close_handshake_session` を追加し、SETUP 交換中の
  失敗で終了コード付きのセッション終了を送るようにした。session が `CloseSession` を積んでいれば
  (`recv_control` / `recv_control_stream_type` / `recv_control_stream_closed` の検証失敗) その
  コードと理由を使い、積んでいなければ (decode 失敗) 渡されたコードと理由を使う。積まれていない
  場合は `Session::close` を呼んでから改めて取り出し、ローカル専用コードの正規化後のコードを使う。
  `CloseSession` はキューの先頭とは限らないため `take_close_session` で見つかるまで取り出す。
  終了コードを wire に載せるのは `StreamHandle::close` (`drain_events` の `CloseSession` 処理と
  同じ経路) であり、QUIC では CONNECTION_CLOSE になる (§6.6 (Termination))。制御ストリームへ
  メッセージを書くのではない。
- decode 失敗の終了コードは `session_error_code` の写像で決める
  (`close_handshake_session_on_message_error`)。`TransportError::Moqt` 以外 (transport 自体の
  失敗) は接続が死んでいるため何もしない。
- `establish_quic` / `establish_wt` / `establish_wt_h2` / `finish_handshake` の失敗経路のうち、
  SETUP の decode 失敗、`recv_control(peer_setup)` の検証失敗、stream type の不一致、SETUP 前の
  control stream 終端を同じ扱いにした。`?` で早期 return していた箇所は `match` に書き換えて
  ヘルパを呼んでから error を返す。stream type の検証は 3 経路で同一だったため
  `recv_control_stream_type` に集約した。
- 終了コードの写像を `examples/tokio-moq/src/error.rs` の
  `pub fn session_error_code(error: &MessageError) -> u64` へ移設し、`moq-pub` / `moq-sub` の
  pipeline は `tokio_moq::error::session_error_code` を参照するようにした (`lib.rs` の方針どおり
  最上位再エクスポートは設けない)。pipeline 側の private 定義は削除し、単体テストは移設先の
  `session_error_code_maps_message_failures` として維持した (アサーション 4 件は同一)。写像の doc
  から「初回 SETUP では終了コードを送れない (既知の限界)」を除き、確立前の SETUP の decode 失敗も
  この写像で決めることを書いた。
- `examples/tokio-moq/src/peer_harness.rs` を拡張した。送る SETUP を差し替える経路 (`send_setup`
  と、control stream の先頭に書く完全なバイト列を渡す `send_control_stream_head`)、SETUP を
  送らずに終端する経路 (`finish_control_stream`)、peer 側で client の CONNECTION_CLOSE の終了
  コードを観測する経路 (s2n-quic の event subscriber の `on_connection_closed` が受け取る
  `connection::Error::Application` を `initiator: Remote` に限って記録する `CloseCodeRecorder`)
  を追加し、module doc の「提供しない」から提供するようになった能力を外した。
- テストは 7 本追加した。トークン構造の decode 失敗 (未知の Alias Type) で
  `KEY_VALUE_FORMATTING_ERROR` (0x6)、トークンの重複 (REGISTER 2 件、alias のみ異なる) で
  `MALFORMED_AUTH_TOKEN` (0x16)、SETUP の Token の DELETE で `PROTOCOL_VIOLATION` (0x3)、
  server 役の SETUP の PATH で `INVALID_PATH` (0x8)、control stream の stream type 不一致、
  stream type を読む前の終端、stream type の直後の終端で `PROTOCOL_VIOLATION` (0x3) を peer 側で
  観測する。生の Setup Options は型付き API で構築できないため、`ControlMessage::Unsupported` で
  SETUP と同じ書式のバイト列を組み立てている。
- 設計方針の「対象外」の理由を実態に合わせた。自側の失敗は peer の違反ではないため対象にせず、
  transport 自体の失敗は接続が死んでいるため送れない。確立後の `pump_once` の `recv_control` /
  `recv_stream_message` の失敗は同じ欠陥クラス (session が `CloseSession` を積んでいるのに `?` が
  `drain_events` より先に return する) だが、確立前とは別経路であるため対象外とし別 issue で扱う
  (`recv_request` は `drain_events` を呼んでおり欠陥は無い)。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。変異テスト
  で、close を送らない変異で 7 本、写像・分岐ごとの変異でそれぞれ対応するテストが失敗すること、
  `initiator` の絞り込みを反転すると終了コードを観測する 7 本が失敗することを確認した。
- 併せて `examples/tokio-moq/src/lib.rs` の
  `resolve_socket_addrs_times_out_when_resolver_does_not_answer` が OS の否定応答キャッシュに
  依存して不安定だった (OS が否定応答をキャッシュすると、100 ナノ秒のタイムアウトより先に解決が
  失敗して timeout 経路を観測できない) ため、テストごとに一意な authority を使うようにした
  (0240 とは別のコミット)。追加したテストでこのキャッシュが起きるようになり push と CI の前提と
  して直した。
