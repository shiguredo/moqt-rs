# request stream GOAWAY の非空 New Session URI で個別 request を移行する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-10-25

## reopened にした理由

複数 session 併走の設計が `docs/multi-session-request-migration.md` にまとまり、実装タスクと完了条件を
具体化できたため、pending から reopened して実装対象に戻した。設計は設計 issue (0376) で行った。

## 目的

draft-ietf-moq-transport-22 §9.2 (GOAWAY) は、request stream 上の GOAWAY を受けた endpoint に "the endpoint SHOULD re-issue that specific
request on a session at the specified URI (or the current session if no URI is provided), and close the old request stream using
the appropriate mechanism (e.g. FIN, stream reset, or PUBLISH_DONE)" を SHOULD で要求する。現在の examples は移行先 URL が現在の URL と
同じときだけ現在の session で当該 request を再発行し、別 URL の非空 URI は warn して移行しない (subscriber)。
publisher は空 URI のときだけ現在の session で PUBLISH を再発行し、非空 URI は同一 URL でも移行しない。`docs/moqt.md` と `docs/missing-features.md` に
残課題として明記されている。

## 現状

- `src/session/types.rs` の `SessionEvent::GoawayReceived` は `new_session_uri` / `timeout` / `on_request_stream: Option<u64>`
  を持ち、request stream 上の GOAWAY を識別できる。
- `examples/moq-sub/src/pipeline.rs` と `examples/moq-pub/src/pipeline.rs` は control GOAWAY を受けたときに新しい session を background
  で確立し、認可トークンの引き継ぎ・期限・指数バックオフ再試行を伴って購読 / 配信を張り直す移行経路を持つ。
- moq-sub は `migration_target` と `same_session_url` により、移行先 URL が現在の URL と同じなら現在の session で当該 request を
  再発行する。別 URL のときは移行しない。moq-pub の `goaway_action` は `new_session_uri` が空かどうかだけで分岐し、
  非空 URI は同一 URL でも `IgnoreRequestScope` として移行しない (本 issue で同一 URL の再発行も対象にする)。
- 両 example の main ループは 1 つの `MoqtClient` と 1 つの `DataPlaneHandle` を所有し、複数 session を同時に扱う経路を持たない。
  moq-sub の `track_map` / `media_fetch_targets`、moq-pub の固定 alias / `PublisherSession` は単一 session の ID 空間を前提にしている。

## 設計方針

設計の全体は `docs/multi-session-request-migration.md` にまとめた。要点は次のとおり。

- session ごとに専用 task が `MoqtClient` を所有し、main ループは `SessionHandle` のコマンド / イベントチャネルで複数 session を
  同時に扱う (`MoqtClient` の送信 API / `tick_now` / `next_event` が `&mut self` のため)。
- `(SessionId, request_id)` の `RequestOwner` で request と session の対応を管理し、移行時に差し替える。
- 移行先 URL が現在の URL と同じなら新 session を作らず現在の session で再発行する (既存挙動の保存)。別 URL のときだけ新 session を
  確立する。同一移行先 URL を指す複数 request は 1 つの新 session を共有する。
- 移行中は新旧 session の Object を論理 track ごとの Location watermark で重複排除し、catalog / decode order の状態は session を
  またいで共有する。
- 旧 request は新 session の受理後に FIN / RESET_STREAM / PUBLISH_DONE の適切な方法で閉じる。期限内に移行できない場合は
  control GOAWAY 移行と同じ警告と終了にする。
- 対象とする request は subscriber の SUBSCRIBE (FILL_PARAMETERS を含む) と FETCH、publisher の PUBLISH とする。
  SUBSCRIBE_NAMESPACE など未対応 request は従来どおり対象外とする。
- ライブラリの `Session` API は変更しない (CODEBASE.md の責務分界)。変更は `examples/tokio-moq` / moq-pub / moq-sub に閉じる。
- `docs/moqt.md` と `docs/missing-features.md` の残課題の記述を実装に合わせて更新する。

## 実装タスク

1. `examples/tokio-moq` に `SessionHandle` / `SessionCommand` / session task / イベント集約の共通基盤を追加する。
2. 2 session を同時に駆動するテスト補助を用意する (0338 の test-support feature を先行するか、example 内の test 補助か)。
3. moq-sub の main ループを `SessionHandle` ベースへ変更し、request 単位 GOAWAY の状態機械と data plane ルーティングを実装する。
4. moq-pub の main ループを `SessionHandle` ベースへ変更し、PUBLISH の request 単位移行と旧 session の PUBLISH_DONE を実装する。
5. 移行中の重複排除 (watermark) と catalog / decode order の共有状態を実装する。
6. `docs/moqt.md` / `docs/missing-features.md` を更新する。
7. テスト (移行成功 / 同一 URL / 失敗 / 重複排除) を追加する。

変更対象ファイルは `examples/tokio-moq/src/` (session 管理の新モジュール、`peer_harness.rs`)、
`examples/moq-sub/src/pipeline.rs`、`examples/moq-pub/src/pipeline.rs`、`docs/moqt.md`、
`docs/missing-features.md` とする。

## 完了条件

- 非空 New Session URI の request stream GOAWAY を受けたとき、移行先 URL が現在の URL と異なる場合は指定 URI の session が確立され、
  当該 request だけが新 session で再発行され、旧 request stream が FIN / RESET_STREAM / PUBLISH_DONE の適切な方法で閉じられること
  (テスト `migrated_request_reissued_on_new_session`)。
- 移行先 URL が現在の URL と同じ場合、subscriber は新 session を作らず現在の session で当該 request が再発行されること
  (既存挙動の保存。テスト `subscriber_same_url_request_goaway_reissued_on_current_session` で検証)。
  publisher は非空 URI の同一 URL でも現在の session で PUBLISH を再発行すること
  (テスト `publisher_same_url_request_goaway_reissued_on_current_session`)。
- 同一移行先 URL を指す複数 request は 1 つの新 session を共有すること
  (テスト `multiple_requests_share_migrated_session`)。
- 旧 session の他の request と data plane が継続すること
  (テスト `old_session_other_requests_continue_during_migration`)。
- 空 URI の request stream GOAWAY は従来どおり現在の session で再発行されること
  (テスト `empty_uri_request_goaway_reissued_on_current_session`)。
- 期限内に移行できない場合、control GOAWAY 移行と同じ警告ログとセッション終了になること
  (テスト `migration_deadline_exceeded_closes_session`)。
- 移行中に旧 session で in-flight だった Object がアプリへ二重に渡らないこと
  (テスト `no_duplicate_objects_during_migration`)。
- `docs/moqt.md` / `docs/missing-features.md` の記述が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
