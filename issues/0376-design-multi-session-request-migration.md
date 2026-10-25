# 複数 session 併走による request 単位の GOAWAY 移行を設計する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

`issues/pending/0375-draft-22-request-stream-goaway-migration.md` の request stream GOAWAY の非空 New Session URI 移行を
実装可能にするため、examples (`moq-pub` / `moq-sub`) の単一 session 前提アーキテクチャを複数 session 併走へ拡張する
設計を `docs/` にまとめる。本 issue は設計のみを対象とし、実装は本 issue の成果物に従って 0375 で行う。

## 現状

- draft-ietf-moq-transport-22 §9.2 (GOAWAY) は request stream 上の GOAWAY を受けた endpoint に、当該 request を
  `new_session_uri` の session (URI が空なら現在の session) で再発行し、旧 request stream を FIN / RESET_STREAM /
  PUBLISH_DONE で閉じることを SHOULD で求める。
- examples の control GOAWAY 移行は session 全体の置換である。`examples/moq-pub/src/pipeline.rs` の
  `migrate_publisher_session` / `spawn_old_session_close` は新 session を背景で確立して main ループの
  `MoqtClient` / `DataPlaneHandle` を差し替え、古い session の PUBLISH を PUBLISH_DONE で終端して NO_ERROR で閉じる。
  request stream GOAWAY は `goaway_action` が空 URI のときだけ現在 session で当該 request を再発行し、非空 URI は
  `IgnoreRequestScope` として諦めている。
- 両 example の main ループは `client` / `data_plane` を 1 組だけ保持し、stream task は 1 つの `DataPlaneHandle`
  (1 つの `Session`) と Track Alias の 1 つの対応表 (`track_map` / `media_fetch_targets`) を共有している。
- fill fetch stream / datagram 受信 / peer request の処理も単一 data plane の request ID 空間を前提にしている。
- examples の peer harness は 1 session のみを扱うため、2 session 併走の移行を検証できない。
- `docs/missing-features.md` は「request stream GOAWAY の非空 URI による単一 request 移行 (6.6) — 2 session の併走が
  必要」と残課題を記載している。

## 設計で決定する項目

1. session 管理: main ループが複数の `MoqtClient` / `Session` を所有し、全 session のイベントを公平に poll する方法。
   各 session の `tick` (data stream timeout / delivery timeout / GOAWAY deadline) を同じループで進める方法。
2. 移行状態機械: 移行対象 request ごとの状態 (移行中 / 新 session 確立済み / 再発行済み / 受理済み / 旧 request 終端済み /
   期限切れ失敗) と、main ループ・背景タスクの責務分担。
3. request と session の対応: どの層 (ライブラリの Session API 拡張、`MoqtClient`、example のラッパー) で
   request ID ごとの session 所有を管理するか。request ID は session ごとに独立して採番されるため、example 内の
   識別子 (request ID 単独ではなく (session, request ID) の組) の扱いを決める。
4. data plane のルーティング: 各 session の Track Alias、受信 subgroup / FETCH data stream の dispatch、fill fetch
   stream (`OpenFillFetchStream`)、datagram、peer request をどのように正しい session / subscription へ振り分けるか。
   `DataPlaneHandle` が単一 Session を指す現設計を維持できるか、拡張が必要かを結論付ける。
5. 重複と順序: 移行中に旧 session で in-flight だった Object と新 session で受信し直した Object の重複排除
   (Track 単位の watermark / Location 比較)、catalog 適用の冪等性、映像・音声の decode order への影響。
6. request 種別ごとの移行手順: subscriber の SUBSCRIBE (FILL_PARAMETERS を含む) / FETCH、publisher の PUBLISH。
   再発行に必要なパラメータ・Track Properties・認可トークンの引き継ぎ、旧 request stream の終端方法。
7. 失敗時と期限: GOAWAY の Timeout までに新 session を確立できない場合、再発行が拒否された場合、新 session が
   途中で閉じた場合の扱い。control GOAWAY 移行の再試行・指数バックオフ・期限の再利用範囲。
8. 観測: 移行中・移行完了・失敗をアプリが観測できるログ、イベント、メトリクス。既存の
   `SessionEvent::GoawayReceived { on_request_stream, new_session_uri, timeout }` で足りるか。
9. テスト: 2 session を同時に扱う peer harness の構成、移行中と移行後の data plane の検証、期限切れ・拒否・
   二重受信の failure injection。

## 成果物

- `docs/` に設計文書 1 本 (現状の単一 session 前提、複数 session 化の設計、移行状態機械、data plane ルーティング、
  重複排除、request 種別ごとの手順、失敗時、テスト方針)。
- 0375 を実装するためのタスク分割 (ライブラリ変更 / `tokio-moq` 変更 / moq-pub / moq-sub / テスト harness) と、
  各タスクの変更対象ファイル一覧。
- 設計文書の結論として、0375 の完了条件の更新案 (実装可能な形に具体化したもの)。

## 完了条件

- 上記「設計で決定する項目」1〜9 がすべて設計文書で決定されていること。
- 0375 の実装が追加設計なしに着手できる粒度のタスク分割と変更対象ファイル一覧があること。
- 0375 の完了条件の更新案が、検証可能な形 (どのテストで何を確認するか) で書かれていること。
- 設計文書は polish 済みで、致命的・重要な指摘が 0 件であること。
- 本 issue ではコードを変更しない (成果物は `docs/` と 0375 の本文更新のみ)。

## 対象外

- 0375 の実装そのもの (本 issue の成果物に従って別途行う)。
- relay 機能の追加 (CODEBASE.md のとおり本プロジェクトは client / server のみを対象とする)。
