# GOAWAY の New Session URI へセッションを移行する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/add-goaway-session-migration
- Polished: 2026-10-15

## 目的

draft-ietf-moq-transport-22 §9.2 (GOAWAY) は、server が New Session URI を提示した場合に
client がその URI を新しい session に使う MUST を定める。§6.6.1 (Graceful Session Migration) は
新しい session を背景で確立し、Established な購読 / 配信を移す手順を定める。

現状の example は GOAWAY を受けると New Session URI を捨てて終了するため、リレーの
計画的再起動や移行のたびに配信が完全に止まる。client endpoint として、control stream の
GOAWAY で session 全体を移行できるようにする。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` は `SessionEvent::GoawayReceived` の
  `new_session_uri` を使わず、`timeout` をログして配信を終了する。
  `examples/moq-sub/src/pipeline.rs` の `run` も同じ
- `SessionEvent::GoawayReceived` は `new_session_uri` / `timeout` / `on_request_stream` を持ち、
  ライブラリは control stream と request stream の GOAWAY を区別して通知できる (`src/session/types.rs`)
- `examples/tokio-moq` の `MoqtClient` は `establish_quic` / `establish_wt` / `establish_wt_h2` で
  接続済みの資源を受け取ってハンドシェイクするだけであり、GOAWAY を受けた後に別 URI へ
  接続し直す経路が無い
- 接続の生成 (アドレス解決、`connect_with_fallback`、transport 分岐 (quic / wt-h3 / wt-h2)、
  `:authority` / `:path` の組み立て) は `examples/moq-pub/src/pipeline.rs` と
  `examples/moq-sub/src/pipeline.rs` に重複して置かれている。両者の差は実装名と、
  moq-sub の WebTransport 分岐だけが呼ぶ datagram の継続受信の有効化である
- 両 example とも終了時に `MoqtClient::send_goaway` へ空の URI を渡しており、client が空 URI を
  送る MUST は満たしている
- 両 example は `on_request_stream` を区別せず、request stream の GOAWAY でも session 全体を
  終了する。§6.6.1 は request stream の GOAWAY を「その request だけを移し、残りの session は
  そのままにする」と定めるため、現状は session を落としすぎている
- I/O を持たない `Session` (`Session::new_client`) は接続の確立と差し替えを扱わない。
  移行の実装場所は transport 層と example になる
- 接続を差し替えると `MoqtClient::data_plane` が返す `DataPlaneHandle` と、
  `MoqtClient::handle` が返す `StreamHandle` も変わる。これらを保持する
  `SubgroupWriter` / `DatagramWriter`、受信タスクへ clone 済みの `DataPlaneHandle` と
  `StreamHandle` (moq-sub の datagram 受信タスクが保持する)、`establish_*` が返す
  `StreamAcceptor` も接続ごとに作り直す必要がある
- `examples/moq-sub/src/pipeline.rs` は `config.namespace` を move 済みで、再確立に同じ
  `config` を使い回せない

## 設計方針

### 移行の判断

- 移行するのは control stream の GOAWAY (`on_request_stream` が `None`) だけとする。
  request stream の GOAWAY の再発行は本 issue では扱わない (対象外を参照)
- request stream の GOAWAY (`on_request_stream` が `Some(request_id)`) では移行せず、
  現在の session と他の request を維持する。当該 request の再発行は行わず、自側からも終端しない。
  受信側の Session は deadline を登録しないため `ResetRequestStream` は発行されない。
  `timeout` が 0 より大きいときは GOAWAY の送信側が §9.2 の SHOULD に従って当該 stream を
  `GOING_AWAY` で reset し、その終端通知で `RequestTerminated` (`PeerStreamReset`) が発行される。
  `timeout` が 0 のときは当該 request は `Established` のまま残る
- `new_session_uri` が非空ならその URI へ、空なら現在の URI を再利用して
  新しい session を確立する (§9.2)
- 移行の判断 (URI の有無、移行の成否) は I/O から独立した関数として pipeline から切り出し、
  単体テストで固定する。I/O の結果 (接続の成否) は入力として受け取る

### 接続の確立

- `examples/tokio-moq` に、URL と接続設定から接続を確立する共通の経路を追加する。
  入力は `ServerUrl`、`Transport`、証明書 (`--cert`)、実装名、datagram の継続受信を
  有効にするか (moq-sub の WebTransport 分岐だけが必要とする)、戻り値は `MoqtClient` と
  `StreamAcceptor` とする。moq-pub / moq-sub に重複している transport 分岐をこの経路へ寄せ、
  初回接続と移行の双方で使う
- 移行先 URI は `parse_url` と同じ規則で解釈し、authority / path (query を含む) は
  移行先 URI のものを使う。scheme は `moqt://` だけを受け付け、それ以外は移行失敗として扱う。
  WebTransport 経路では URI の scheme 置換を行わず、WT 層が `:scheme=https` を組み立てる
  既存の規則をそのまま使う
- 現在の設定から引き継ぐのは次のものに限る
  - 接続経路 (`--transport`)。移行先 URI の `msf` fragment の `connection` と矛盾する場合は
    移行失敗として扱う (`resolve_transport` と同じ整合規則)
  - 証明書 (`--cert`)
  - C4M 認可トークン。移行先 URI の fragment に `c4m` が無いときは元の `--url` の fragment の
    トークンを使う。移行先でも同じ track の PUBLISH / SUBSCRIBE を張り直すため、
    `AUTHORIZATION_TOKEN` を載せ続ける必要がある (draft-ietf-moq-msf-01 §11.4.3)
  - Track Namespace (`--namespace` と `msf` fragment)

### 移行の順序と旧 session の終了

- 新しい session を先に確立し、確立できてから購読 / 配信を移す (§6.6.1)。
  移行中もキャプチャとエンコードは止めない
- publisher は新しい session で同じ Track Namespace / Track Name を `publish_track` で
  PUBLISH し直す。`examples/moq-pub/src/group_id.rs` が担う Group ID の単調増加は移行をまたいで
  維持する
- subscriber は SUBSCRIBE を張り直す。catalog は `catalog_subscribe_parameters` と
  `receive_catalog` の経路をやり直す
- `DataPlaneHandle` / `StreamHandle` / `StreamAcceptor` と、それらを保持する writer および
  受信タスクは、新しい接続のものへ作り直す
- 旧 session の移行対象の購読 / 配信を STOP_SENDING / PUBLISH_DONE で終了する。この送信自体は
  `SessionEvent::RequestTerminated` を発行しないため、その遷移を待たずに close へ進む
  (peer が stream を閉じた時点で `PeerStreamFin` / `PeerStreamReset` として発行されうるが、
  example のアプリには配送されない)。publisher は PUBLISH_DONE の前に、開くすべての stream を
  閉じて送るべき datagram が残っていない状態にする (§9.9 の MUST)
- 移行対象がすべて `Terminated` になった時点で `close(0, "")` で閉じる (§6.6.1)。
  `timeout` が 0 のときは期限を設けず、0 より大きいときは timeout の経過を
  旧 session を閉じる期限として扱う (§9.2 の Timeout)
- 移行先の確立に失敗した場合は、従来どおり旧 session の終了経路で終了する

### 対象外

- request stream 単位の GOAWAY による request の再発行 (§9.2 の SHOULD)。
  現在の session を維持したまま特定の request だけを移す必要があり、
  受信ループが 2 つの session を併走させる設計が要るため、別 issue で扱う
- 0-RTT (§6.3.1)
- 移行中に欠落した Object の補填 (FETCH)
- 実 relay を使う E2E。GOAWAY を送らせる手段が無いため、relay 側の検証基盤は別途扱う

## 完了条件

- New Session URI 付きの control stream GOAWAY を受けた moq-pub / moq-sub が移行先へ接続し、
  PUBLISH / SUBSCRIBE を張り直して配信 / 購読を継続すること
- New Session URI が空の control stream GOAWAY では現在の URI を再利用して同じことを行うこと
- 旧 session の移行対象の購読 / 配信を STOP_SENDING / PUBLISH_DONE で終了し、
  すべて `Terminated` になった時点で `close(0, "")` で閉じること
- request stream の GOAWAY では session と他の request が維持されること
- 移行先へ接続できないときは従来どおり終了すること
- 差し替え後に datagram の受信 (moq-sub) と data stream の送受信が新しい接続で継続すること
- 移行の判断が I/O から独立した関数として切り出され、moq-pub / moq-sub の単体テストで
  固定されていること
- 共通の接続確立経路と、移行先 session での PUBLISH / SUBSCRIBE の成立が
  `examples/tokio-moq/src/peer_harness.rs` のテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
