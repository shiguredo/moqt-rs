# request stream GOAWAY の非空 New Session URI で個別 request を移行する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.2 (GOAWAY) は、request stream 上の GOAWAY を受けた endpoint に "the endpoint SHOULD re-issue that specific
request on a session at the specified URI (or the current session if no URI is provided), and close the old request stream using
the appropriate mechanism (e.g. FIN, stream reset, or PUBLISH_DONE)" を SHOULD で要求する。現在の examples は空 URI のときだけ現在の session で当該
request を再発行し、非空 URI は warn して移行しない。`docs/moqt.md` と `docs/missing-features.md` に残課題として明記されている。

## 現状

- `src/session/types.rs` の `SessionEvent::GoawayReceived` は `new_session_uri` / `timeout` / `on_request_stream: Option<u64>`
  を持ち、request stream 上の GOAWAY を識別できる。
- `examples/moq-sub/src/pipeline.rs` と `examples/moq-pub/src/pipeline.rs` は control GOAWAY を受けたときに新しい session を background
  で確立し、認可トークンの引き継ぎ・期限・指数バックオフ再試行を伴って購読 / 配信を張り直す移行経路を持つ。
- request stream GOAWAY は空 URI のときだけ現在の session で当該 request を再発行し、非空 URI では「2 session の併走が必要」として warn を残して諦めている。
- 両 example は 1 本の session を前提に data plane (Track Alias / fill fetch stream / datagram) を構成しており、複数 session を同時に扱う経路を持たない。

## 設計方針

- 非空 URI の request stream GOAWAY を受けたら、既存の control GOAWAY 移行の接続確立・認可トークン引き継ぎ・期限・再試行の仕組みを使って指定 URI の session を追加で確立し、当該 request
  だけを新 session で再発行する。
- 旧 session は維持し、他の request と data plane を継続させる。新 session で発行した request の data plane (Track Alias / fill fetch stream / datagram)
  は request 単位で新 session 側へ振り分ける。
- 対象とする request は subscriber の SUBSCRIBE (FILL_PARAMETERS を含む) と FETCH、publisher の PUBLISH とする。SUBSCRIBE_NAMESPACE など未対応 request
  は従来どおり対象外とする。
- 移行が完了したら旧 request stream を FIN / RESET_STREAM / PUBLISH_DONE のうち適切な方法で閉じる。期限までに移行できない場合は従来どおり警告して終了する。
- `docs/moqt.md` と `docs/missing-features.md` の残課題の記述を実装に合わせて更新する。
- テストは `examples/tokio-moq` の peer harness を使い、2 session を併走させて非空 URI の request GOAWAY 後に当該 request だけが新 session に現れ、旧 session
  の他の request が継続することを固定する。

## 完了条件

- 非空 New Session URI の request stream GOAWAY を受けたとき、指定 URI の session が確立され、当該 request だけが新 session で再発行されること。
- 旧 session の他の request と data plane が継続すること。
- 空 URI の request stream GOAWAY は従来どおり現在の session で再発行されること。
- 期限内に移行できない場合の挙動とログが既存の control GOAWAY 移行と整合すること。
- `docs/moqt.md` / `docs/missing-features.md` の記述が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
