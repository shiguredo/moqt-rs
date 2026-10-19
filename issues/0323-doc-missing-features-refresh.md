# docs/missing-features.md を現コードに同期させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-missing-features-refresh
- Polished: {YYYY-MM-DD}

## 目的

docs/missing-features.md は 2026-10-14 の f91baf3 時点のスナップショットのまま更新されておらず、その後の実装と逆の「未対応」記述が残っている。実装状況の把握に使うと誤った判断を導くため、現コードに同期させる。

## 現状

- 「MoqtClient に FETCH API が無い」→ `examples/tokio-moq/src/moqt_client.rs` の `fetch` が実在する。
- 「MAX_CACHE_DURATION は examples に出現しない」→ `examples/moq-pub/src/pipeline.rs` が `MAX_CACHE_DURATION` を告知する。
- 「メディアの FETCH に応答しない / 保持しない」→ `examples/moq-pub/src/pipeline.rs` の `serve_media_fetch` と `examples/moq-pub/src/cache.rs` が応答する。
- 「moq-sub は FETCH を発行しない」→ `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` が発行する。
- 「両 example とも new_session_uri を無視して終了する」→ `examples/moq-pub` / `examples/moq-sub` の `migration_target` と `resubscribe_tracks` が移行する。
- 存在しない `--audio-datagram` を案内している箇所がある (実在するのは `--audio-subgroup`)。
- 行番号参照が広範囲にずれており、参照先として機能していない。
- issue ファイルパスへの参照が含まれ、docs への issue 参照禁止に反する。
- playout の既知の制限 (44.1 kHz 非対応、時間伸長なし、音声再生中心など) の記載が無い。

## 設計方針

- 各節を現コードと突き合わせて更新し、解消済みの項目を削除する。
- 行番号ではなくファイルパスとシンボル名で参照する。
- issue パスの列を削除し、理由を本文に書く。
- playout の既知の制限の節を追加する。
- スナップショットとして残す場合は基準 commit と日付を明記し、履歴であることを示す。

## 完了条件

- 現コードと逆の記述が無いこと。
- 行番号参照がシンボル名参照に置き換わっていること。
- docs 内に issue 番号 / issue ファイルパスの参照が無いこと。
