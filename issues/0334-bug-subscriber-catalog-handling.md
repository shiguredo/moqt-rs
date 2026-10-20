# moq-sub のカタログ解釈を MSF に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscriber-catalog-handling
- Polished: 2026-10-20

## 目的

moq-sub のカタログ解釈に、MSF-01 の MUST と実装上の取りこぼしが 2 つある。

1. track 選択が codec の一致だけで `packaging` / `role` を見ない。MSF は media を LOC で配送し
   (§4.1 (LOC packaging))、media track の codec は §5.2.18 (Codec) が定める。packaging が `loc` でない
   track に media codec が付いたカタログを受理すると、非 LOC の payload を LOC として購読し、
   デコードに失敗し得る。選択するメディア種別と異なる予約 role の track も同じく購読対象から外す
   必要がある (§5.2.6 (Track role))。
2. 独立カタログ (Full) の置き換えで、旧カタログとの `isLive` / `isComplete` の履歴を検証しない。
   `isLive=false` の後に `isLive=true` の track を受理し (§5.2.7 (Is Live) の "A True value MUST never
   follow a False value.")、`isComplete` が true のカタログで新しい track を追加したり、`isComplete` を
   落としたりする (§5.1.3 (Is Complete) の "This field MUST NOT be removed from a catalog once it has
   been added." と "no new tracks will be added to the catalog")。

`isLive=false` の `targetLatency` (§5.2.8 (Target latency)) は本 issue の対象外である。`MsfCatalogDocument::decode` の
`decode_track` / `decode_clone_track` / `MsfCloneTrack::into_track` が `target_latency` を `None` に
正規化しており、moq-sub が受け取るカタログは必ずこの経路を通る。この判断は closed の
[0220](closed/0220-bug-msf-target-latency-render-group.md) と
[0104](closed/0104-add-subscriber-av-sync.md) で確認済みであり、`tests/test_msf/track_fields.rs` の
`target_latency_ignored_when_is_live_false` が固定している。example 側で `is_live` を再確認する必要はない。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `receive_catalog` は、映像 track を codec が `av01` / `avc1` /
  `hvc1` / `hev1` で始まるか、`vp8` / `vp09` と一致するか (または `vp09.` で始まるか) だけで選び、
  音声 track を `decoder::is_supported_audio_codec` だけで選ぶ。`MsfTrack::packaging` と
  `MsfTrack::role` は条件にしていない。
- `MsfTrack::packaging` は `MsfPackaging` (必須) であり、`loc` 以外に `mediatimeline` / `eventtimeline` /
  `moqlog` / `moqmetrics` も値として持つ。`src/msf.rs` の `validate_media_track_fields` は media codec の
  track に `bitrate` を要求するが、`packaging` が `loc` であることは要求しない。このため、`packaging` が
  `mediatimeline` / `eventtimeline` / `moqlog` / `moqmetrics` の track に `codec: "av01..."` /
  `codec: "opus"` と `bitrate` を付けたカタログは decode を通り (`mediatimeline` / `eventtimeline` では
  さらに `depends` / `mimeType` などの必須フィールドを満たせばよい)、`receive_catalog` がその track を
  LOC の映像 / 音声として購読する。payload は LOC ではないため、デコードに失敗し得る。
- `examples/moq-sub/src/catalog.rs` の `CatalogState::apply` は、独立カタログ (Full) を受けるたびに `self.catalog = Some(full.clone());` で置き換え、旧カタログの track との `isLive` / `isComplete` の比較をしない。
- `src/msf.rs` の `MsfCatalog::apply_delta` は delta 内の再追加で `isLive` の false→true を拒否するが、独立カタログ間の履歴は保持しない。`MsfCatalog` の `removed_tracks` は 1 つのカタログインスタンス内の delta 用であり、Full の置き換えで新しい `MsfCatalog` に差し替わると履歴は失われる (closed の [0127](closed/0127-bug-msf-delta-track-attribute-change.md) が限界として記録している)。
- 影響: 非 LOC トラックを LOC として購読してデコードに失敗し得る。`isLive=false`→`true` の Full カタログ遷移と、`isComplete` を落とす / `isComplete=true` の後に track を追加する Full カタログ遷移を受理する。MSF の MUST に違反するカタログ遷移を受理する。

## 設計方針

### track 選択

- `receive_catalog` の映像 / 音声の選択部分を純粋関数 `select_video_track` / `select_audio_track` に切り出し、`#[cfg(test)] mod tests` から直接テストできるようにする。両関数は `&[MsfTrack]` から条件に合う最初の track を返す。
- 映像 track の条件:
  - `track.packaging == MsfPackaging::Loc`
  - `track.codec` が本 example の対応映像 codec (`av01` / `avc1` / `hvc1` / `hev1` は登録名で始まること、`vp8` は完全一致、`vp09` は完全一致または `vp09.` で始まること。現在の判定を維持する)
  - `track.role` が予約された非映像 role (`audio` / `audiodescription` / `mediatimeline` / `eventtimeline` / `caption` / `subtitle` / `log` / `metrics`) でないこと。`role` が無い場合と custom role は codec で判定する。`video` と `signlanguage` は映像として扱う (§5.2.6 (Track role) の予約 role 一覧)。
- 音声 track の条件:
  - `track.packaging == MsfPackaging::Loc`
  - `track.codec` が `decoder::is_supported_audio_codec` を満たすこと
  - `track.role` が予約された非音声 role (`video` / `signlanguage` / `mediatimeline` / `eventtimeline` / `caption` / `subtitle` / `log` / `metrics`) でないこと。`role` が無い場合と custom role は codec で判定する。`audio` と `audiodescription` は音声として扱う (§5.2.6 (Track role) の予約 role 一覧)。
- 候補が複数ある場合は先頭を選ぶ。選択した track から `extract_video_info` / `extract_audio_info` が失敗した場合は従来どおりエラーにする (幅 / 高さ / samplerate / channelConfig を欠く track の fallback は本 issue の範囲外)。映像と音声の両方が無い場合も従来どおりエラーにする。
- `targetLatency` の決定 (`catalog_target_latency_ms`) とテスト用の `track_with_target_latency` は変更しない。`isLive=false` の `targetLatency` は decode で `None` に正規化済みである。

### 独立カタログの履歴検証

- `CatalogState` に、セッション中に観測した track の履歴を追加する。
  - `seen_not_live: HashSet<(String, String)>`: 解決済みの `(namespace, name)` ごとに、`isLive=false` を観測したことを記録する。
  - `is_complete_seen: bool`: `isComplete=true` のカタログを適用したことを記録する。
- track のキーは `MsfTrack::namespace` があればその文字列、無ければ `CatalogState::catalog_namespace` を使う (§5.2.2 (Track namespace) の継承)。delta の namespace 継承と同じ規則である。
- Full カタログの適用前に、次の順で検証する。1 つでも違反したら `Err` を返し、`catalog` / `latest_group` / `last_applied` / 履歴を更新しない (呼び出し側は警告ログを出し、起動時の取得ではエラーとして終了する)。
  1. `is_complete_seen` が true で、新しい Full の `is_complete` が false なら拒否する (§5.1.3 の "MUST NOT be removed")。
  2. `is_complete_seen` が true で、新しい Full に適用済みカタログの track キーに無い track があれば拒否する (§5.1.3 の "no new tracks will be added")。
  3. 新しい Full の各 track について、`is_live` が true かつ `seen_not_live` にキーがあれば拒否する (§5.2.7 の "A True value MUST never follow a False value")。
- 検証を通った Full を適用したら、履歴を更新する。新しい Full の `is_live=false` の track キーを `seen_not_live` に追加し、`is_complete` が true なら `is_complete_seen` を true にする。
- delta の適用は、適用前の `self.catalog` の複製に対して行い、履歴検証を通った場合だけ `self.catalog` を差し替える。
- `MsfCatalog::apply_delta` は現在のカタログインスタンスの `removed_tracks` しか見ないため、Full の置き換えで履歴が失われた後の delta 再追加は `seen_not_live` で検出する。適用後のカタログの track が `is_live=true` かつ `seen_not_live` にキーを持てば拒否する (§5.2.7 の "A True value MUST never follow a False value")。
- `is_complete_seen` が true のときは、適用後のカタログの track キーがすべて適用前の `self.catalog` の track キーに含まれることも確認する (§5.1.3 の "no new tracks will be added")。通常は `MsfCatalog::apply_delta` の `is_complete` 検査で拒否されるが、`CatalogState` の履歴と一致させる。
- 検証を通ったら、適用後のカタログの `tracks` から `is_live=false` の track キーを `seen_not_live` に追加し、`is_complete` が true なら `is_complete_seen` を true にする。拒否した delta の track は履歴に記録しない。
- 拒否した Full の track は履歴に記録しない (適用していないカタログの状態を履歴に混ぜない)。
- エラーメッセージには、違反した MUST (§5.2.7 または §5.1.3) と、track 単位の違反であれば対象の track キーを含める。

## 完了条件

- 映像 / 音声の選択で、`packaging` が `loc` でない track が選ばれないこと。
- `packaging` が `loc` の track でも、選択するメディア種別と異なる予約 role の track が映像 / 音声として選ばれないこと。`role` 無し / custom role / `video` / `signlanguage` / `audio` / `audiodescription` の扱いが上記の規則どおりであること。
- Full カタログで `isLive=false`→`true` の track が拒否され、旧カタログが維持されること。
- Full カタログで `isLive=true`→`false` の track と、`isComplete=false`→`true` の遷移が受理されること。
- Full カタログで `isComplete` が true から false (省略) に変わった場合に拒否されること。
- `isComplete=true` の後に track を追加した Full カタログが拒否されること。
- `isComplete=true` の後に track を追加しない Full カタログが受理されること。
- delta で追加された `isLive=false` の track を、後の Full で `isLive=true` にした場合に拒否されること。
- Full の置き換えで履歴が失われた後に、delta で同じ track を `isLive=true` で再追加した場合に拒否されること。
- namespace を省略した track と catalog namespace を明示した track が同じキーとして扱われること。
- Full / delta の拒否時にカタログ状態 (`catalog` / `latest_group` / `last_applied`) と履歴が更新されないこと。
- 上記を `examples/moq-sub/src/pipeline.rs` と `examples/moq-sub/src/catalog.rs` の `#[cfg(test)] mod tests` に単体テストとして固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
