# cmaf の prft で音声と映像を壁時計の時間軸に載せる

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: {YYYY-MM-DD}

## 目的

moq-sub は、壁時計の Timestamp を持つフレームだけを時間軸に載せる。それ以外のフレームは届いた順に表示する。0471 / 0472 の cmaf では、時刻が tfdt (メディア時刻) で伝わるので `TimestampKind::MediaTime` になる。その結果、live capture でも音声と映像の同期と遅延の追従が働かない。

本 issue では、live capture の cmaf の chunk に prft (Producer Reference Time Box) を付け、media time と壁時計の対応を伝える。moq-sub はそれを読んで、LOC の壁時計 Timestamp と同じ時間軸に載せる。

前提は 0471 (moq-pub) と 0472 (moq-sub) である。CMSF 対応全体の方針は 0467 を参照すること。

## 現状

- `crates/moq-sub/src/main.rs` の `wall_clock_timestamp` / `wall_clock_pts` は、`TimestampKind::WallClock` のときだけ時刻を返す。
- `crates/moq-sub/src/pipeline.rs` の `receive_input_lag_us` / `log_audio_end_to_end_lag` も、壁時計の Timestamp でしか働かない。
- `shiguredo_mp4` (2026.6.0-canary.2) には prft の型がない。`Fmp4SegmentDemuxer` は moof の前の prft を解釈せずに読み飛ばす。同 crate のコメントは、prft を ISO/IEC 14496-12 の 8.16.5 としている。
- ISO/IEC 14496-12 の本文は `refs/` にない。

## 設計方針

- moq-pub (live capture の cmaf だけ):
  - 各 chunk の moof の前に prft を置く。prft には、その chunk の media time (tfdt) と capture 時刻 (NTP 形式の壁時計) の対応を書く。
  - CMSF §3.3 は moof + mdat を 1 組以上含むことを要求するだけで、他の box を禁じていない。
  - `--input-mp4` / `--input-mp4-reencode` には壁時計の capture 時刻がないので、prft は付けない。
- prft の構造:
  - 規格本文が `refs/` にないため、実装に着手する前に ISO/IEC 14496-12 §8.16.5 の本文で box の構造を確認する。本文を確認できない場合は着手しない。その場合は、shiguredo-issues の規約に従い、理由を追記して pending にする。
  - box のバイト列は moq-pub / moq-sub で組み立て、解析する。`shiguredo_mp4` への型の追加は本 issue の範囲外とする。
- moq-sub:
  - 0472 が組ごとに分けた payload で、moof の直前に prft があれば読む。
  - sample の PTS を、prft の media time と壁時計の対応から Unix epoch のマイクロ秒に変換し、`TimestampKind::WallClock` として扱う。こうすると、既存の時間軸と端末間遅延ログに載る。
  - 入力遅れの計測 (`receive_input_lag_us`) は、LOC の Timestamp / Timescale を引数に取り、Timescale の有無で壁時計かどうかを判断している。`TimestampKind::WallClock` を付けるだけでは計測に載らない。cmaf でも使えるよう、引数を壁時計の時刻 (または `TimestampKind`) を受け取る形に変える。
  - prft がない Object (他の publisher や file 入力) は、0472 のとおり `TimestampKind::MediaTime` のままにする。

## 完了条件

- live capture の cmaf では、Object が prft + moof + mdat の形になること。`--input-mp4` / `--input-mp4-reencode` では prft を付けないこと。
- prft の media time が tfdt と一致し、壁時計が capture 時刻と一致すること。
- moq-sub が、prft のある Object を `TimestampKind::WallClock` の時刻で表示すること。prft のない Object は `TimestampKind::MediaTime` のままであること。
- prft のある cmaf の Object で、`receive_input_lag_us` による入力遅れが計測されること。
- prft の組み立てと解析を、moq-pub / moq-sub の単体テストで往復させて固定すること。
- `docs/msf.md` の CMSF の節に、prft の使い方を追記すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
