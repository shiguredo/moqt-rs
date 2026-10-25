# 複数 session 併走による request 単位の GOAWAY 移行を設計する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-10-25

## 目的

`issues/0375-draft-22-request-stream-goaway-migration.md` の request stream GOAWAY の非空 New Session URI 移行を
実装可能にするため、examples (`moq-pub` / `moq-sub`) の単一 session 前提アーキテクチャを複数 session 併走へ拡張する
設計を `docs/` にまとめ、0375 を reopened にして実装タスクを明記する。本 issue は設計と 0375 の更新のみを対象とし、
実装は 0375 で行う。

## 現状

- draft-ietf-moq-transport-22 §9.2 (GOAWAY) は request stream 上の GOAWAY を受けた endpoint に、当該 request を
  `new_session_uri` の session (URI が空なら現在の session) で再発行し、旧 request stream を FIN / RESET_STREAM /
  PUBLISH_DONE で閉じることを SHOULD で求める。
- control GOAWAY の移行は session 全体の置換である。`examples/moq-pub/src/pipeline.rs` の `migrate_publisher_session`
  は新 session を確立して `MigratedSession` を返し、main ループの `migration_outcome` 分岐が `client` / `data_plane`
  を差し替える。`spawn_old_session_close` は旧 session の PUBLISH を PUBLISH_DONE で終端して NO_ERROR で閉じる。
- request stream GOAWAY の現状は publisher と subscriber で異なる。
  - `examples/moq-pub/src/pipeline.rs` の `goaway_action` は、空 URI なら現在の session で当該 PUBLISH を再発行し、
    非空 URI は `IgnoreRequestScope` として警告して移行しない。
  - `examples/moq-sub/src/pipeline.rs` は `migration_target` と `same_session_url` を使い、request stream GOAWAY の
    移行先 URL が現在の URL と同じなら現在の session で当該 request (`goaway_request_kind` / `reissue_request`) を
    再発行する。別 URL のときは移行しない。
- 両 example の main ループは 1 つの `MoqtClient` と 1 つの `DataPlaneHandle` を所有する。moq-sub は Track Alias の
  対応表 `track_map` (alias → TrackKind) と `media_fetch_targets` (fetch request ID → TrackKind) を持ち、stream task は
  1 つの `DataPlaneHandle` とこれらの表を共有する。moq-pub は両表を持たず、固定 alias
  (`VIDEO_TRACK_ALIAS` / `AUDIO_TRACK_ALIAS`) と `PublisherSession` の request ID、`data_plane` を共有する。
- `examples/tokio-moq/src/peer_harness.rs` の `goaway_migration_switches_to_the_new_session` は 2 つの
  `TestPeerServer` / `TestPeer` / `MoqtClient` を同時に保持して control GOAWAY 移行を検証済みである。ただし
  `peer_harness` は `#[cfg(test)]` モジュールであり、moq-pub / moq-sub のテストからは参照できない (test-support
  feature 化は 0338 で計画)。2 session を同時に駆動する補助 API は無い。
- `docs/missing-features.md` は「request stream GOAWAY の非空 URI による単一 request 移行 (6.6) — 2 session の
  併走が必要」と残課題を記載している。

## 制約

- CODEBASE.md のとおり、`Session` は 1 本の MOQT Transport Session に閉じた endpoint-local の protocol state machine で
  あり、Sans I/O / no_std を守る。本設計でライブラリの `Session` API を複数 session 対応に拡張しない
  (session の所有・ルーティングは examples と `examples/tokio-moq` の範囲に閉じる)。
- relay 機能は追加しない。モック・スタブは使わず、失敗系も実 peer の REQUEST_ERROR / 無応答 / 切断で作る。
- `issues/pending/` の issue は編集しない規約のため、0375 は本 issue の中で「reopened にした理由を追記して
  `issues/` へ移動」してから本文を更新する。

## 設計で決定する項目

1. session 管理: 複数の `MoqtClient` を同時に所有し、各 session のイベントを公平に処理する方法。`MoqtClient` の
   送信 API / `tick_now` / `next_event` が `&mut self` であるため、main ループが clients を直接保持して
   `next_event` を回す方式は成立しない。session ごとに専用タスクが `MoqtClient` を所有し、main ループは
   コマンド (送信 / tick / 停止) とイベントをチャネルでやり取りする方式を採る。
2. request と session の対応: `(SessionId, request_id)` を作った `RequestOwner` で管理し、論理 track ごとに 1 つの
   所有者を持つ。移行時に所有者を差し替える。
3. data plane のルーティング: subscriber 側 (moq-sub) は session ごとの `track_map` / `media_fetch_targets` を持ち、
   受信 stream task に `(SessionId, DataPlaneHandle, alias 表)` を渡す。publisher 側 (moq-pub) は固定 alias と
   `PublisherSession` の request ID を session ごとに分離する。fill fetch stream / datagram / peer request も
   受信した session の `MoqtClient` / `DataPlaneHandle` で処理する。
4. 移行状態機械: request ごとの状態 (対象受信 / 新 session 確立中 / 再発行済み / 応答待ち / 完了 / 失敗)、
   main ループと背景タスクの責務、旧 request の終端、期限切れの扱い。control GOAWAY 移行の再試行・指数バックオフ・
   期限の実装を共通化する。
5. 同一 URL の扱い: `migration_target` で解決した移行先 URL が現在の session の URL と同じなら、新 session を
   作らず現在の session で再発行する (moq-sub の `same_session_url` の既存挙動を保存する)。別 URL のときだけ
   新 session を確立する。非空 URI でも同一 URL なら現 session 再利用とする判断を 0375 の完了条件に反映する。
6. 同一移行先 URL を指す複数 request: 同一 URL への移行は 1 つの新 session を共有する。moq-sub は catalog /
   video / audio の 3 request を持ち、同じ GOAWAY URI が届くことがあるため、URL ごとにセッション確立を 1 回に
   まとめ、各 request はその session で再発行する。
7. 重複と順序: 移行中に旧 session で in-flight だった Object と新 session で受信し直した Object の重複排除
   (論理 track ごとの Location watermark)、catalog 適用の冪等性、映像・音声の decode order への影響。
8. request 種別ごとの移行手順: subscriber の SUBSCRIBE (FILL_PARAMETERS を含む) / FETCH、publisher の PUBLISH。
   再発行に必要なパラメータ・Track Properties・認可トークンの引き継ぎ、旧 request stream の終端方法。
9. 失敗時と観測: 期限切れ・再発行拒否・新 session 切断・旧 request 終端失敗の扱い。移行中 / 完了 / 失敗を
   アプリが観測できるログとイベント (既存の `SessionEvent::GoawayReceived` で足りるか)。
10. テスト: 2 session を同時に扱う harness の構成 (0338 の test-support feature を前提とするか、example 内の
    test 補助か)、移行中と移行後の data plane の検証、期限切れ・拒否・二重受信の failure injection。

## 成果物

1. `docs/` の設計文書 1 本。上記 1〜10 の決定と、subscriber / publisher それぞれの変更設計、移行状態機械、
   data plane ルーティング、重複排除、request 種別ごとの手順、失敗時、観測、テスト方針を含む。
2. 0375 の reopened と本文更新 (完了条件・設計方針・タスク分割・実装対象ファイル)。0375 は `issues/` へ戻す。
3. 実装タスク分割: ライブラリ変更なしを前提に、`examples/tokio-moq` (session 管理 / harness)、moq-pub、moq-sub、
   テストの変更対象ファイル一覧と順序。

## 完了条件

- 設計文書に上記 1〜10 の決定がすべて書かれ、0375 の実装が追加設計なしに着手できる粒度のタスク分割と変更対象
  ファイル一覧があること。
- 設計文書が `/review-doc` で致命的・重要 0 件であること (polish-issue は issue 専用のため docs には使わない)。
- 0375 が reopened にした理由を本文へ追記したうえで `issues/` へ移動され、完了条件が検証可能な形
  (どのテストで何を確認するか) に更新され、設計文書とタスク分割を参照していること。
- 本 issue ではコードを変更しない (成果物は `docs/` と 0375 の更新のみ)。

## 対象外

- 0375 の実装そのもの (本 issue の成果物に従って 0375 で行う)。
- ライブラリの `Session` の複数 session 対応 (CODEBASE.md の責務分界に反するため)。
- relay 機能の追加 (CODEBASE.md のとおり client / server のみを対象とする)。
