# GOAWAY の New Session URI へセッションを移行する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/add-goaway-session-migration
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.2 (GOAWAY) は、server が New Session URI を提示した場合に
client がその URI を新しい session に使う MUST を定める。§6.6.1 (Graceful Session Migration) は
新しい session を確立してから購読 / 配信を移す手順を示す。

現状の example は GOAWAY を受けると New Session URI を捨てて終了するため、リレーの
計画的再起動や移行のたびに配信が完全に止まる。client endpoint として移行に追随できるようにする。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` は `SessionEvent::GoawayReceived` の
  `new_session_uri` を使わず、`timeout` をログして配信を終了する。
  `examples/moq-sub/src/pipeline.rs` の `run` も同じ
- `SessionEvent::GoawayReceived` は `new_session_uri` / `timeout` / `on_request_stream` を持ち、
  ライブラリは control stream と request stream の GOAWAY を区別して通知できる (`src/session/types.rs`)
- `examples/tokio-moq` の `MoqtClient` は `establish_quic` / `establish_wt` / `establish_wt_h2` で
  1 接続を確立するだけであり、GOAWAY を受けた後に別 URI へ接続し直す経路が無い
- 両 example とも終了時に `MoqtClient::send_goaway` へ空の URI を渡しており、client が空 URI を
  送る MUST は満たしている
- request stream 上の GOAWAY で該当 request だけを再発行する経路 (§9.2 の SHOULD) も無い
- I/O を持たない `Session` (`Session::new_client`) は接続の確立と差し替えを扱わない。
  移行の実装場所は transport 層と example になる

## 設計方針

- `MoqtClient` に「新しい URI へ接続して session を差し替える」経路を追加する。
  New Session URI が空のときは現在の URI を再利用する (§9.2)
- 移行先も `moqt://` として解釈し、`--transport` / `--cert` / `--url` の query など現在の接続設定を
  引き継ぐ。WebTransport 経路では `moqt` URI から `https` URI への scheme 置換を既存の経路と同じ
  規則で行う
- publisher (moq-pub) は新しい session で同じ Track Namespace / Track Name を `publish_track` で
  PUBLISH し直す。`examples/moq-pub/src/group_id.rs` が担う Group ID の単調増加は移行をまたいで
  維持する。キャプチャとエンコードは移行中も止めない
- subscriber (moq-sub) は SUBSCRIBE を張り直す。catalog は `catalog_subscribe_parameters` と
  `receive_catalog` の経路をやり直す
- request stream 単位の GOAWAY (`on_request_stream` が `Some`) は、その request だけを
  移行先 session で再発行する
- 移行先の確立に失敗した場合は従来どおり終了する。旧 session の終了処理
  (PUBLISH_DONE / GOAWAY / close) は現状の終了経路を流用する
- 0-RTT (§6.3.1) と、移行中に欠落した Object の補填 (FETCH) は本 issue の対象外とする
- テストは `examples/tokio-moq/src/peer_harness.rs` の `TestPeerServer` / `TestPeer` を拡張し、
  New Session URI 付きの GOAWAY を送ったうえで 2 本目の接続で PUBLISH / SUBSCRIBE が成立することを
  固定する。実 relay が要る確認は `e2e-tests/` に `#[ignore]` 付きで置く

## 完了条件

- New Session URI 付きの GOAWAY を受けた moq-pub / moq-sub が移行先へ接続し直し、
  配信 / 購読を継続すること
- New Session URI が空の GOAWAY では現在の URI を再利用すること
- 移行先へ接続できないときは従来どおり終了すること
- request stream 単位の GOAWAY で該当 request が再発行されること
- 上記が `examples/tokio-moq/src/peer_harness.rs` のテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
