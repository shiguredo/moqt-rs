# docs/missing-features.md を現コードに同期させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-doc-missing-features-refresh
- Polished: 2026-10-20

## 目的

docs/missing-features.md は 2026-10-14 の f91baf3 時点の調査メモとして 2026-10-15 に追加され、2026-10-16 の 0288 で QMux の節だけが追記された。他の節は f91baf3 時点のままで、その後の実装と逆の「未対応」記述が残っている。実装状況の把握に使うと誤った判断を導くため、現コードに同期させ、基準 commit と日付を明記したスナップショットとして維持する。あわせて playout の既知の制限を追記する。

## 変更対象

- `docs/missing-features.md` のみ。
- コード (`src/`, `examples/`) と README は変更しない。

## 現状

以下は確認済みの代表例であり、これに限らない。設計方針の全数突き合わせで他の古い記述も見つけ次第更新する。

### docs/missing-features.md が現コードと逆になっている箇所

- 「`MoqtClient` に発行 API が無い (削除済み)」(3 章の表) と「`MoqtClient` にも発行 API が無い」(6.1) → `examples/tokio-moq/src/moqt_client.rs` の `fetch` が実在し、`examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` が購読直後に直近 Group を FETCH する。3 章の「過去の Object を取得できない」「切断後の追いつきができない」は、直近 Group を FETCH できるようになった点が逆で、シーク / 巻き戻しは現状もできない。
- 「`MAX_CACHE_DURATION` は `examples/` に出現しない」(5.5) → `examples/moq-pub/src/pipeline.rs` の
  `cache_duration_properties` が `PROP_MAX_CACHE_DURATION` を PUBLISH と SUBSCRIBE_OK で告知する。
  同じ節が挙げる `DEFAULT_PUBLISHER_PRIORITY` / `DEFAULT_PUBLISHER_GROUP_ORDER` /
  `IMMUTABLE_PROPERTIES` / `EXPIRES` は現状も Track Property として告知していない。ただし
  `DEFAULT_PUBLISHER_PRIORITY` は定数として `examples/moq-pub/src/pipeline.rs` に出現するため、
  「`examples/` に出現しない」という記述自体も正しくない。
- 「メディアの FETCH に応答しない / 保持しない」(5.8) → `examples/moq-pub/src/pipeline.rs` の `serve_media_fetch` と `examples/moq-pub/src/cache.rs` の `MediaCache` が FETCH 要求に応答し、`--cache-duration` の窓で保持する。同じ節の「fill fetch は catalog のみで、映像 / 音声は reset する」は現状も正しい。
- 「moq-sub は FETCH を発行しない」(6.1) → `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` が直近 Group を FETCH する。途中参加でキーフレームまで遡ることはできるようになったが、受信が途切れた区間の埋め直しと VOD の任意位置からの再生は現状もできない。メディアの fill fetch を要求しない点も現状のままである。
- 「両 example とも `new_session_uri` を無視して終了する」(6.6) → `examples/moq-pub/src/pipeline.rs` の `establish_publisher_session` と `examples/moq-sub/src/pipeline.rs` の `resubscribe_tracks` が移行する。同じ節の「request stream 上の GOAWAY による個別 request の再発行には対応しない」は現状も正しい。
- 「Largest Object はログのみで fill range の判断に使わない」(6.11) → `fetch_recent_groups` が `SubscribeResult::largest_object` から FETCH の range を決める。ただし fill fetch の range の判断には使わないという記述は現状も正しい。「PUBLISH_DONE はログのみ」(6.11) も、現在はログを出して example を終了するため逆である。Object Status の扱い (payload_length == 0 を一律 skip) は現状も正しい。
- 行番号参照が広範囲にずれており、参照先として機能していない。例: 4.2 の `examples/moq-sub/src/pipeline.rs` の行番号参照は、現在 `TrackKind` の定義を指しており、`targetLatency` の処理を指していない。
- issue 番号 / issue ファイルパス / issue への言及が含まれ、`shiguredo-issues` の「issue 番号や issue への言及を書いてはいけない場所」に反する。2 章の表の issue 列、2 章の「issue 起票済み」、4.1 / 4.7 / 7 章 / 10 章の issue 参照、10 章の「URL 系の issue」が該当する。

### 追加する記述

- docs/missing-features.md には playout の既知の制限が無い。`skills/shiguredo-moqt/SKILL.md` には
  対応レート (8 kHz / 16 kHz / 32 kHz / 48 kHz) と時間伸長なしが記載済みで、
  `src/playout/stretch.rs` の `is_time_stretch_available` は 44.1 kHz を非対応と明記しているが、
  docs/missing-features.md には出てこない。moq-sub の節に、時間圧縮と補間は
  8 kHz / 16 kHz / 32 kHz / 48 kHz のみで 44.1 kHz は働かないこと、時間伸長 API は無いこと、
  映像表示は example 側で playout は音声再生が中心であることを追記する。

## 対象外

- `--audio-datagram` という存在しないフラグを `examples/moq-pub/src/error.rs`、`examples/moq-sub/README.md`、`examples/moq-sub/src/pipeline.rs` が案内している問題は、コード / README 側の修正であり、0324 が扱う。docs/missing-features.md 8 章の記述は現状も正しいため、本 issue では現状のまま残し、参照形式だけ整える。
- docs/moqt.md の「音声の時間圧縮・伸長」という記述と `skills/shiguredo-moqt/SKILL.md` の自己矛盾は 0324 が扱う。本 issue では触らない。
- 0308 が扱う §4.7、0309 が扱う §4.1、0289 が扱う 7 章の QMux は、本 issue では現時点のコードに合わせた記述と参照形式だけを整え、解消済みとして削除しない。解消扱いはそれぞれの issue に委ねる。

## 設計方針

- 冒頭の「調査日: 2026-10-14 (develop の f91baf3 時点)」を、同期に使った develop の commit と日付に更新する。
- 本ファイルはその commit 時点のスナップショットであり、以後のコード変更には追随しないことと、最新の実装状況はコードを参照することを冒頭に明記する。
- 各節を基準 commit のコードと突き合わせ、解消済みの項目を削除する。解消していない項目は残し、記述を現コードに合わせる。
- 行番号ではなくファイルパスとシンボル名で参照する。シンボル名で一意に特定できない場合は逐語引用を併記する。
- issue 番号 / issue ファイルパス / issue への言及をすべて削除し、必要な理由は本文に書く。2 章の表の issue 列は削除する。
- playout の既知の制限を moq-sub の節に追加する。
- コード / README は変更しない。

## 完了条件

- `docs/missing-features.md` の冒頭に、同期に使った develop の commit と日付、スナップショットであること、最新の実装状況はコードを参照する旨が明記されていること。
- 基準 commit のコードと逆の記述が無いこと。
- 行番号参照がファイルパスとシンボル名 (または逐語引用) の参照に置き換わっていること。
- `docs/missing-features.md` 内に issue 番号 / issue ファイルパス / issue への言及が無いこと。
- playout の既知の制限が moq-sub の節に記載されていること。
- markdownlint-cli2 が通ること。
