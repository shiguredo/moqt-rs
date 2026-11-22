# docs/missing-features.md を現コードに同期させる

- Created: 2026-10-19
- Completed: 2026-11-22
- Branch: feature/update-doc-missing-features-refresh
- Polished: 2026-11-20

## 目的

docs/missing-features.md は 2026-10-14 の f91baf3 時点の調査メモとして 2026-10-15 に追加され、その後複数回更新されている。6.6 や 6.15 などは更新済みだが、3 章、5.5、5.8、6.1、6.11 などには、その後の実装と逆の「未対応」記述が残っている。実装状況の把握に使うと誤った判断を導くため、現コードに同期させ、基準 commit と日付を明記したスナップショットとして維持する。あわせて playout の既知の制限を追記する。

## 変更対象

- `docs/missing-features.md` のみ。
- コード (`src/`, `crates/`) と README は変更しない。

## 現状

以下は確認済みの代表例であり、これに限らない。設計方針の全数突き合わせで他の古い記述も見つけ次第更新する。

### docs/missing-features.md が現コードと矛盾している箇所

- 「`MoqtClient` に発行 API が無い (削除済み)」(3 章の表) と「`MoqtClient` にも発行 API が無い」(6.1) → `crates/tokio-moq/src/moqt_client.rs` の `MoqtClient::fetch` (`pub async fn fetch`) が実在し、`crates/moq-sub/src/pipeline.rs` の `fetch_recent_video_groups` が映像の購読直後に直近 Group を FETCH する (音声の明示 FETCH は発行しない)。
  3 章の「過去の Object を取得できない」「切断後の追いつきができない」は、直近 Group を FETCH できるようになった点が逆で、シーク / 巻き戻しは現状もできない。
- 「`MAX_CACHE_DURATION` は `crates/` に出現しない」(5.5) → `crates/moq-pub/src/pipeline.rs` の
  `cache_duration_properties` が `PROP_MAX_CACHE_DURATION` を PUBLISH と SUBSCRIBE_OK で告知する。
  同じ節が挙げる `DEFAULT_PUBLISHER_PRIORITY` / `DEFAULT_PUBLISHER_GROUP_ORDER` /
  `IMMUTABLE_PROPERTIES` は現状も Track Property として告知していない。`DEFAULT_PUBLISHER_PRIORITY`
  は定数として、`IMMUTABLE_PROPERTIES` はコメントやテストの定数参照として `crates/` に出現するため、
  「`crates/` に出現しない」という記述自体も正しくない。`EXPIRES` は Track Property ではなく
  PUBLISH の Message Parameter であり、コメントとして `crates/` に出現するため、5.5 の Track Property
  の列挙に混ぜるのは誤りである。
- 「メディアの FETCH に応答しない / 保持しない」(5.8) → `crates/moq-pub/src/pipeline.rs` の `serve_media_fetch` と `crates/moq-pub/src/cache.rs` の `MediaCache` が FETCH 要求に応答し、`--cache-duration` の窓で保持する。同じ節の「fill fetch は catalog のみで、映像 / 音声は reset する」は現状も正しい。
- 「moq-sub は FETCH を発行しない」(6.1) → `crates/moq-sub/src/pipeline.rs` の `fetch_recent_video_groups` が映像の直近 Group を FETCH する。音声の明示 FETCH は発行しない。6.1 の「ライブ配信の途中参加時に、キーフレームまで遡って表示を開始する」も、この FETCH により可能になった。受信が途切れた区間の埋め直しと VOD の任意位置からの再生は現状もできない。メディアの fill fetch を要求しない点も現状のままである。
- 6.6 は GOAWAY 移行の実装に合わせて更新済みであり、`crates/moq-pub/src/pipeline.rs` の `establish_publisher_session` と `crates/moq-sub/src/pipeline.rs` の `resubscribe_tracks` の移行、request stream 上の GOAWAY による再発行が docs に反映されている。本 issue の対象外である。
- 「Largest Object はログのみで fill range の判断に使わない」(6.11) → `fetch_recent_video_groups` が `SubscribeResult::largest_object` から映像 FETCH の range を決める。音声は Largest Object を購読境界として使い、FETCH は発行しない。
  ただし fill fetch の range の判断には使わないという記述は現状も正しい。「PUBLISH_DONE はログのみ」(6.11) も逆である。
  `publish_done_disposition` が status code を `PUBLISH_DONE_TRACK_ENDED` / `PUBLISH_DONE_GOING_AWAY` / エラーに分類し、購読の終了待ちに入れて drain 完了後に track を終了させる。全 media track が恒久終了し、移行待ちも無ければ example を終了する。
  `stream_count` の突き合わせは Session の `pending_publish_done` / `stream_count_overrun` が行う。Object Status の扱い (payload_length == 0 を一律 skip) は現状も正しい。
- 行番号参照が広範囲にずれており、参照先として機能していない。例: 4.2 の `crates/moq-sub/src/pipeline.rs:727-743` は現在 `pcm_duration_us` を指し、`2150-2175` は `MigrationFailure` を指しており、`targetLatency` の処理 (`catalog_target_latency_ms` / `target_latency_ms`) を指していない。
- issue 番号 / issue ファイルパス / issue への言及が含まれ、`shiguredo-issues` の「issue 番号や issue への言及を書いてはいけない場所」に反する。2 章の表の issue 列、2 章の「issue 起票済み」、10 章の `issues/pending/0010` と「URL 系の issue」が該当する。4.1 / 4.7 / 7 章には現状 issue 参照は無い。

### 追加する記述

- docs/missing-features.md の 6.15 には playout の適用層と復号前遅延の未移植が書かれているが、
  対応サンプルレートと 44.1 kHz の扱い、時間伸長の API 名、映像表示が example 側であることは
  書かれていない。`skills/shiguredo-moqt/SKILL.md` は 8 kHz / 16 kHz / 32 kHz / 48 kHz /
  44.1 kHz に対応し、44.1 kHz は解析用の信号だけを 48 kHz の格子へ換算すると記載している。
  `src/playout/stretch.rs` の `is_time_stretch_available` は 44.1 kHz を true で返し、
  `preemptively_expand` が時間伸長を行う。moq-sub の節に、対応レート、44.1 kHz の解析用扱い、
  時間伸長が `preemptively_expand` であること、映像表示は example 側で playout は音声再生が
  中心であることを追記する。

## 対象外

- `--audio-datagram` という存在しないフラグを `crates/moq-pub/src/error.rs`、`crates/moq-sub/README.md`、`crates/moq-sub/src/pipeline.rs` が案内している問題は、コード / README 側の修正であり、0324 が扱う。docs/missing-features.md 8 章の記述は現状も正しいため、本 issue では現状のまま残し、参照形式だけ整える。
- docs/moqt.md の「音声の時間圧縮・伸長」という記述と `skills/shiguredo-moqt/SKILL.md` の自己矛盾は 0324 が扱う。本 issue では触らない。
- 0308 / 0309 / 0289 は closed 済みである。docs/missing-features.md の 4.7 / 4.8 は解消済みなので削除する。4.1 は残るギャップを残し、7 章の QMux は制約として残す。記述を現コードに合わせ、参照形式を整える。

## 設計方針

- 冒頭の「調査日: 2026-10-14 (develop の f91baf3 時点)」を、同期に使った develop の commit と日付に更新する。「最終更新」行は削除し、「調査日」に同期日を一本化する。
- 本ファイルはその commit 時点のスナップショットであり、以後のコード変更には追随しないことと、最新の実装状況はコードを参照することを冒頭に明記する。
- 各節を基準 commit のコードと突き合わせ、解消済みの項目を削除する。解消していない項目は残し、記述を現コードに合わせる。0308 / 0309 / 0289 は closed 済みであり、4.7 / 4.8 は解消済みとして削除する。4.1 は残るギャップを残し、7 章は制約として残す。
- 5.5 の `EXPIRES` は Track Property ではなく PUBLISH の Message Parameter であるため、Track Property の列挙から分ける。
- 行番号ではなくファイルパスとシンボル名で参照する。シンボル名で一意に特定できない場合は逐語引用を併記する。
- issue 番号 / issue ファイルパス / issue への言及をすべて削除し、必要な理由は本文に書く。2 章の表の issue 列は削除する。
- playout の既知の制限を moq-sub の節 (6.15) に追加する。
- コード / README は変更しない。

## 完了条件

- `docs/missing-features.md` の冒頭に、同期に使った develop の commit と日付、スナップショットであること、最新の実装状況はコードを参照する旨が明記されていること。
- 基準 commit のコードと矛盾する記述 (逆の記述、存在しない API の参照、Track Property と Message Parameter の混同など) が無いこと。
- 行番号参照がファイルパスとシンボル名 (または逐語引用) の参照に置き換わっていること。
- `docs/missing-features.md` 内に issue 番号 / issue ファイルパス / issue への言及が無いこと。
- playout の対応サンプルレート (8 kHz / 16 kHz / 32 kHz / 48 kHz / 44.1 kHz)、44.1 kHz は解析用の信号だけを 48 kHz の格子へ換算すること、時間伸長が `preemptively_expand` であること、映像表示は example 側であることが moq-sub の節 (6.15) に記載されていること。
- markdownlint-cli2 が通ること。

## 解決方法

- 冒頭を「基準: develop の `08f38d2` (2026-11-22) 時点のスナップショット」とし、
  最新の実装状況はコードを参照する旨を明記した。
- `MoqtClient::fetch` と `fetch_recent_video_groups` の実装に合わせて FETCH の節を更新し、
  直近 Group の取得はできるがシーク / 巻き戻し / 任意区間の後追い / VOD はできないと整理した。
- `cache_duration_properties` による `MAX_CACHE_DURATION` の告知を反映し、未告知の
  Track Property を `DEFAULT_PUBLISHER_PRIORITY` / `DEFAULT_PUBLISHER_GROUP_ORDER` /
  `IMMUTABLE_PROPERTIES` / `EXPIRES` に限定した。
- ソースの行番号参照をファイルパスとシンボル名の参照に置き換え、issue 番号・issue への
  言及を削除した。
- playout の節に、対応サンプルレート (8 kHz / 16 kHz / 32 kHz / 48 kHz / 44.1 kHz)、
  44.1 kHz は解析信号だけを 48 kHz の格子へ換算すること、時間伸長が
  `preemptively_expand` であること、映像の表示が example 側であることを追記した。

`prek run --files docs/missing-features.md` (markdownlint を含む) が成功した。
