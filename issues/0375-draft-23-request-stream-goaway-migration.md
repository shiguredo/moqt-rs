# request stream GOAWAY の非空 New Session URI で個別 request を移行する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-11-14
- Refreshed: 2026-11-04

## reopened にした理由

複数 session 併走の設計が `docs/multi-session-request-migration.md` にまとまり、実装タスクと完了条件を
具体化できたため、pending から reopened して実装対象に戻した。設計は設計 issue (0376) で行った。

## 目的

draft-ietf-moq-transport-23 §9.2 (GOAWAY) は、request stream 上の GOAWAY を送れるのは request の receiver だけで
("Only the receiver of a request can send GOAWAY on its request stream.")、request stream 上の GOAWAY を受けた
request の sender に "the sender of the request SHOULD re-issue that specific request on a session at the specified
URI (or the current session if no URI is provided), and close the old request stream using the appropriate mechanism
(e.g. FIN, stream reset, or PUBLISH_DONE)" を SHOULD で要求する。現在の crates は移行先 URL が現在の URL と
同じときだけ現在の session で当該 request を再発行し、別 URL の非空 URI は warn して移行しない (subscriber)。
publisher は空 URI のときだけ現在の session で PUBLISH を再発行し、非空 URI は同一 URL でも移行しない。`docs/moqt.md` と `docs/missing-features.md` に
残課題として明記されている。

## 現状

- `src/session/types.rs` の `SessionEvent::GoawayReceived` は `new_session_uri` / `timeout` / `on_request_stream: Option<u64>`
  を持ち、request stream 上の GOAWAY を識別できる。
- `src/session/goaway.rs` は draft-ietf-moq-transport-23 §9.2 (GOAWAY) の役割規則を実装済みである。
  `send_goaway_on_request_stream` は自側が request の receiver の場合だけ request stream 上の GOAWAY の送信を許可し、
  `handle_peer_goaway_on_request_stream` は自側が request の receiver のときに request stream 上の GOAWAY を受けると
  `PROTOCOL_VIOLATION` でセッションを閉じる。本 issue が移行を扱うのは、自側が送信した request
  (SUBSCRIBE / FETCH / PUBLISH) の stream で受け取る GOAWAY に限られる。
- `crates/moq-sub/src/pipeline.rs` と `crates/moq-pub/src/pipeline.rs` は control GOAWAY を受けたときに新しい session を background
  で確立し、認可トークンの引き継ぎ・期限・指数バックオフ再試行を伴って購読 / 配信を張り直す移行経路を持つ。
- moq-sub は `migration_target` と `same_session_url` により、移行先 URL が現在の URL と同じなら現在の session で当該 request を
  再発行する。別 URL のときは移行しない。`--subscribe-tracks` で受理した PUBLISH 起点の購読
  (`ignored_publish_requests` / `TrackKind::Discard`) は自側からは再発行できないため、同一 URL でも再発行しない。
  moq-pub の `goaway_action` は `new_session_uri` が空かどうかだけで分岐し、非空 URI は同一 URL でも
  `IgnoreRequestScope` として移行しない (本 issue で同一 URL の再発行も対象にする)。
- 両 example の main ループは 1 つの `MoqtClient` と 1 つの `DataPlaneHandle` を所有し、複数 session を同時に扱う経路を持たない。
  moq-sub の `track_map` (alias → `TrackKind`) と `fetch_targets` (fetch request ID → `FetchTarget`)、moq-pub の固定 alias /
  `PublisherSession` は単一 session の ID 空間を前提にしている。

## 設計方針

設計の全体は `docs/multi-session-request-migration.md` にまとめた。要点は次のとおり。

- session ごとに専用 task が `MoqtClient` を所有し、main ループは `SessionHandle` のコマンド / イベントチャネルで複数 session を
  同時に扱う (`MoqtClient` の送信 API / `tick_now` / `next_event` が `&mut self` のため)。
- `(SessionId, request_id)` の `RequestOwner` で request と session の対応を管理し、移行時に差し替える。
- 移行先 URL が現在の URL と同じなら新 session を作らず現在の session で再発行する (subscriber の既存挙動の保存)。
  publisher は非空 URI の同一 URL でも現在の session で PUBLISH を再発行する (新規実装)。別 URL のときだけ新 session を
  確立する。同一移行先 URL を指す複数 request は 1 つの新 session を共有する。
- 移行中は新旧 session の Object を論理 track ごとの Location watermark で重複排除し、catalog / decode order の状態は session を
  またいで共有する。
- 旧 request は新 session の受理後に FIN / RESET_STREAM / PUBLISH_DONE の適切な方法で閉じる。期限内に移行できない場合は
  control GOAWAY 移行と同じ警告を出して main ループを終了する。旧 request stream の `GOING_AWAY` reset は GOAWAY を送った
  peer が `send_goaway_on_request_stream` の `request_stream_deadlines` で行うため、自側は reset を待たずに終了する。
- 対象とする request は subscriber の SUBSCRIBE (FILL_PARAMETERS を含む) と FETCH、publisher の PUBLISH とする。
  SUBSCRIBE_NAMESPACE と SUBSCRIBE_TRACKS 由来の PUBLISH は従来どおり対象外とする。
- `SessionHandle` は example 固有の型に依存させない。alias / request の対応表は example 側に置き、tokio-moq 側は
  `SessionId` / コマンド / イベント / URL だけを持つ。`TrackKind` / `FetchTarget` は moq-sub の private 型であり、
  tokio-moq から参照すると tokio-moq → moq-sub の依存が必要になる。
- FETCH も対象に含める。`fetch_targets` の値は `FetchTarget` だけでは再発行に足りない (range は
  `fetch_recent_video_groups` のローカル変数で組み立てられ、保持されていない) ため、再発行に必要な namespace / track name /
  パラメータを持つ型へ拡張し、キーを `(SessionId, request_id)` にする。移行時は保持した要求で同じ FETCH を再発行する。
- 音声の Audio Config は、0335 で追加する移行成功時のリセット関数を新しい移行経路からも呼ぶ。
- 検証は、tokio-moq 内の `SessionHandle` テストと example の純粋関数の単体テストで行う。`peer_harness` は
  `crates/tokio-moq` の `#[cfg(test)]` からそのまま使えるため、feature の追加や example 側への複製はしない。
  pipeline 全体を 2 session で駆動する統合テストは 0338 に委ね、本 issue の完了条件には含めない。委譲先の 0338 の完了条件には、
  publisher の request stream GOAWAY での PUBLISH 再発行、同一移行先 URL を指す複数 request の session 共有、
  移行期限切れでの main ループ終了、FETCH の再発行、request 単位移行後の Audio Config 再送を追加する。
- ライブラリの `Session` API は変更しない (CODEBASE.md の責務分界)。変更は `crates/tokio-moq` / moq-pub / moq-sub に閉じる。
- `docs/moqt.md` と `docs/missing-features.md` の残課題の記述を実装に合わせて更新する。

## 依存

- 先行: [0335](0335-bug-goaway-audio-config-reset.md)。移行成功時に Audio Config の送信・検証状態をリセットする関数を
  追加し、本 issue の新しい移行経路からも呼ぶ。

## 実装タスク

1. `crates/tokio-moq` に `SessionHandle` / `SessionCommand` / session task / イベント集約の共通基盤を追加する
   (`TrackKind` に依存しない)。2 session を同時に駆動するテストは同一クレートの `#[cfg(test)]` から `peer_harness` を使って書く。
2. moq-sub の main ループを `SessionHandle` ベースへ変更し、request 単位 GOAWAY の状態機械、`(SessionId, request_id)` の
   `RequestOwner`、`fetch_targets` の session 分割と要求保持、data plane ルーティングを実装する。
   SUBSCRIBE (FILL_PARAMETERS を含む) と FETCH を対象とする。
3. moq-pub の main ループを `SessionHandle` ベースへ変更し、PUBLISH の request 単位移行と旧 session の PUBLISH_DONE を実装する。
   非空 URI の同一 URL も現在の session で再発行する。
4. 移行中の重複排除 (watermark) と catalog / decode order の共有状態を実装する。
5. Audio Config のリセットを新しい移行経路からも呼ぶ。
6. `docs/moqt.md` / `docs/missing-features.md` を実装に合わせて更新する。`docs/multi-session-request-migration.md` は
   §1.2 / §3.3 の `fetch_targets`、§3.1 の `SessionHandle` の型 (`TrackKind` 依存の解消)、§3.2 の `RequestOwner` と
   FETCH の要求保持、§3.4 の期限切れ時の reset 主体、§3.5 の request 数、§3.7 の Audio Config (現状は記述なし)、
   §3.10 / §4 のテスト方針 (feature ではなく tokio-moq の `#[cfg(test)]` と example の純粋関数、統合テストは 0338)、
   §5 の受入基準 (reset 主体、FETCH 再発行、Audio Config、0338 への委譲) を実装に合わせて更新する。
7. 単体テストを追加する (tokio-moq の 2 session 駆動、example の純粋関数)。

変更対象ファイルは `crates/tokio-moq/src/` (session 管理の新モジュール。テスト補助が必要なら `peer_harness.rs`)、
`crates/moq-sub/src/pipeline.rs`、`crates/moq-pub/src/pipeline.rs`、`docs/moqt.md`、`docs/missing-features.md`、
`docs/multi-session-request-migration.md` とする。

## 完了条件

- 非空 New Session URI の request stream GOAWAY を受けたとき、移行先 URL が現在の URL と異なる場合は指定 URI の session が確立され、
  当該 request だけが新 session で再発行され、旧 request stream が FIN / RESET_STREAM / PUBLISH_DONE の適切な方法で閉じられること。
- 移行先 URL が現在の URL と同じ場合、subscriber は新 session を作らず現在の session で当該 request が再発行されること。
  publisher は非空 URI の同一 URL でも現在の session で PUBLISH が再発行されること。
- 同一移行先 URL を指す複数 request は 1 つの新 session を共有すること。
- 旧 session の他の request と data plane が継続すること。
- 空 URI の request stream GOAWAY は従来どおり現在の session で再発行されること。
- 期限内に移行できない場合、control GOAWAY 移行と同じ警告ログを出して main ループが終了すること。reset は GOAWAY を送った
  peer が行うため、自側は reset を待たずに終了し、Session を閉じないこと。
- 移行中に旧 session で in-flight だった Object がアプリへ二重に渡らないこと。
- FETCH の request stream GOAWAY でも、保持した namespace / track name / パラメータで同じ FETCH が新 session に再発行されること。
- 移行後の新しい session で、最初にフィルタを通過して送信される音声 Object に Audio Config が載り、subscriber が検証すること。
- `docs/moqt.md` / `docs/missing-features.md` / `docs/multi-session-request-migration.md` の記述が実装と一致すること。
- `crates/tokio-moq` の `#[cfg(test)]` に、`peer_harness` を使って 2 session を同時に駆動するテストがあり、コマンド送信・
  イベント集約・session 終了が検証されていること。
- example の移行判定・watermark・Audio Config リセットの純粋関数に単体テストがあること。
- pipeline 全体を 2 session で駆動する統合テストは 0338 に委ね、本 issue では追加しない。委譲先の 0338 には、publisher の
  request stream GOAWAY での PUBLISH 再発行、同一移行先 URL を指す複数 request の session 共有、移行期限切れでの
  main ループ終了、FETCH の再発行、request 単位移行後の Audio Config 再送を完了条件として追加する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
