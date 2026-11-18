# msf の gzip 圧縮を MSF_COMPRESSION property signaling 機構に追従させる

- Priority: Medium
- Created: 2026-06-30
- Model: Opus 4.7
- Branch: feature/update-msf-draft-02
- Polished: 2026-11-18
- Updated: 2026-11-18

## 経緯

draft-ietf-moq-msf-01 時点では MSF_COMPRESSION の Track Property ID が IANA 未割当 (TBD) であり、Object Property の ID も仕様上確定していなかったため pending にした。draft-ietf-moq-msf-02 §12.1.1 / §12.1.2 は Track Property と Object Property の type をいずれも 0x78 に確定し、§14.3 は 0x78 を [MOQT] §8.4 の application-specific range (0x78-0x7F) とし、IANA の割当を要求しないと明記している。これにより wire 実装の根拠が揃ったため reopened にした。本文は draft-02 の節番号と要件に同期済みである。

## 目的

draft-ietf-moq-msf-02 §12.1 (Compression Signaling) の MSF_COMPRESSION property signaling 機構に追従する。現実装は gzip magic byte 検出による implicit な圧縮判定で、draft-02 の Track Property / Object Property を一切参照していない。draft-02 peer との相互運用で signaling 不整合が生じうるため挙動追従が必要。

動機:

- draft-02 §7.1 (Media Timeline track payload) / §8.2 (Event Timeline data format) は payload を `MAY be compressed using the MSF_COMPRESSION property (Section 12.1)` と規定する。
- draft-02 §12.1 は Track Property (§12.1.1) と Object Property (§12.1.2) の 2 つの mutually exclusive な signaling mechanism を定義する。
- §12.1.1 は `If used, publishers MUST include the MSF_COMPRESSION track property in the PUBLISH message (publisher-initiated flow) or SUBSCRIBE_OK or FETCH_OK (subscriber-initiated flows).` と MUST 規定する。
- §5.5 (Catalog Compression) は catalog も §12.1 の対象とし、全 catalog object を圧縮する場合は Track Property、一部のみ圧縮する場合は Object Property を使うと規定する。

## 優先度根拠

Medium。draft-02 仕様準拠と peer との相互運用性に影響するため Low ではない。一方、現実装の magic byte 検出は gzip payload を展開できるが、property signaling に追従していない。緊急の堅牢性問題ではない。

## 現状

- `src/msf.rs` の Timeline gzip 圧縮・復号関連
  - `TimelineEncodingOptions`: `pub struct { pub gzip: bool }` - MSF_COMPRESSION property に紐付かない bool フラグ
  - `is_gzip_compressed`: gzip magic byte `0x1F 0x8B` 検出のみ
  - `maybe_decompress_gzip`: magic byte 検出による自動展開 (ストリーミング展開、上限 16 MiB の防御は実装済み)
  - `maybe_compress_gzip`: `TimelineEncodingOptions.gzip` で圧縮可否を判定
  - `encode_media_timeline` / `decode_media_timeline` / `encode_event_timeline` / `decode_event_timeline`: 圧縮判定は引数の `TimelineEncodingOptions` のみで signal していない
- `MsfCatalogDocument::encode` / `decode` は JSON のエンコード / デコードだけを行い、gzip を扱わない。`decode` は gzip payload を UTF-8 として解釈できず `InvalidCatalog` を返す。
- `src/track_properties.rs` の `TrackProperties` に MSF_COMPRESSION Track Property (§12.1.1) の定数・アクセサが無い。
- `src/object_properties.rs` の `ObjectProperties` に MSF_COMPRESSION Object Property (§12.1.2) のアクセサが無い。
- `src/property_scope.rs` の `known_property_scope` は 0x78 を未知 property として扱う。このため `TrackProperties::validate` / `ObjectProperties::validate` のスコープ検証と多重検証が 0x78 に適用されない。
- セッション層 (`src/session/subscription/recv.rs` / `send.rs` / `src/session/data.rs` / `src/session/fetch.rs`) は TrackProperties / ObjectProperties を汎用 property として授受するが、MSF_COMPRESSION 固有の解釈・検証は行っていない。`Subscription::peer_track_properties` と `Fetch::track_properties` は Track Properties を保持し、Object の properties bytes は受信 Object に保持される。
- publisher が送出した Track Properties は `Subscription` に保持されない (`peer_track_properties` は peer 発行値のみ)。このため Object 送信時に Track Property と Object Property の同時設定をセッション層で検証するには、自側が送出した MSF_COMPRESSION を保持する必要がある。
- `src/msf.rs` の JSON モデル (`MsfTrack` / `MsfCloneTrack` / `MsfCatalog` / `MsfMediaTimelineEntry` / `MsfEventTimelineEntry`) に MSF_COMPRESSION のフィールドは無い。MSF_COMPRESSION は MOQT 層 property であり (§12)、JSON モデルへの追加は対象外である。
- `tests/test_msf/timeline_gzip.rs` と `pbt/tests/prop_msf.rs` の gzip テストは magic byte 自動判定を前提にしている。`fuzz/fuzz_targets/fuzz_decode_msf_timeline_gzip.rs` も signal を受け取らない。
- `docs/msf.md` と `docs/missing-features.md` は -01 時点の TBD を前提にした記述のままである。

## 設計方針

### 0. 前提: MSF_COMPRESSION property の輸送レイヤ

draft-02 §12 は「MSF uses MOQT Track Properties and Object Properties (see [MOQT]) to signal metadata about MSF tracks and objects. These properties are carried in MOQT control messages and object headers, allowing endpoints to learn track and object characteristics before processing payload data.」と規定する。
MSF_COMPRESSION は **MOQT 層の Track Property / Object Property** であり、catalog JSON (§5.1 / §5.2) や timeline JSON (§7.1 / §8.2) のメンバとして定義されたフィールドは存在しない。
したがって本 issue の統合点は `src/msf.rs` の JSON モデル (`MsfTrack` / `MsfCloneTrack` / `MsfCatalog` / 各 Entry) ではなく、MOQT 層の `src/track_properties.rs` (`TrackProperties`) と `src/object_properties.rs` (`ObjectProperties`)、および payload codec である。
`src/msf.rs` の codec 関数は圧縮 signal を引数で受け取る純粋関数のままにし、JSON モデルにはフィールドを追加しない。

### 1. Property Type 0x78 と IANA

draft-02 §12.1.1 は MSF_COMPRESSION Track Property の type を 0x78、§12.1.2 は Object Property の type を 0x78 と規定する。
§14.3 は 0x78 を [MOQT] §8.4 の application-specific range (0x78-0x7F) とし、IANA registration is not permitted と明記している。IANA の割当を待つ必要はない。
0x78 を実装し、Property Type が application-specific range に属し IANA 登録不要であること、および将来の draft 改版で変わりうることをコードコメントに明記する。

### 2. データモデルへの MSF_COMPRESSION 値型追加

- compression algorithm の値型を Table 14 (MSF Compression Values) / Table 16 (MSF Compression Algorithms registry の初期値) に基づき `MsfCompression` enum として `src/msf.rs` に定義する: `None` (0) / `Gzip` (1)。§12.1「All MSF implementations MUST support both uncompressed payloads (value 0 or property absent) and GZIP compressed payloads (value 1)」により 0 と 1 は全実装が MUST support。
- 未知値 (2 以上) の扱い: §14.4 で値 2-127 は Standards Action、128 以上は private use。`MsfCompression::try_from(u64)` は 2 以上で `MessageError::UnsupportedCompression(value)` を返す。未知値を `Unknown(u64)` として上流に流して処理を継続するのは §12.1.1 / §12.1.2 の MUST NOT attempt to process に反するため採用しない。
- `src/track_properties.rs` に `PROP_MSF_COMPRESSION: u64 = 0x78` 定数と `TrackProperties::msf_compression() -> Option<u64>` アクセサ (`TrackProperties::dynamic_groups` と同型) を追加する。
- `src/object_properties.rs` に `ObjectProperties::msf_compression() -> Option<u64>` アクセサを追加する。
- `src/property_scope.rs` の `known_property_scope` に 0x78 を `PropertyScope::Both` として登録する。これにより `TrackProperties::validate` / `ObjectProperties::validate` のスコープ検証と多重検証 (単一インスタンス) が 0x78 に適用される。
- `src/msf.rs` の JSON モデルにはフィールドを追加しない (MSF_COMPRESSION は MOQT 層 property のため)。

### 3. encode/decode 経路の刷新

- `src/msf.rs` の `TimelineEncodingOptions` を廃止し、`encode_media_timeline` / `decode_media_timeline` / `encode_event_timeline` / `decode_event_timeline` は `MsfCompression` を引数で受け取る。`MsfCompression::None` は非圧縮、`Gzip` は gzip 圧縮・復号を行う。
- `MsfCatalogDocument::encode` / `decode` も `MsfCompression` を引数で受け取る。`None` は非圧縮、`Gzip` は gzip 圧縮・復号を行う。全呼び出し側 (`crates/moq-pub` / `crates/moq-sub` / tests / pbt / fuzz) を新しいシグネチャに更新する。
- `maybe_decompress_gzip` は signal が `Gzip` の場合に magic byte を再検出せず必ず展開する。signal が `None` の場合は展開しない。`Gzip` signal なのに gzip として復号できない場合は `MessageError::GzipDecode` を返す。
- `maybe_compress_gzip` は `MsfCompression::Gzip` の場合に gzip 圧縮し、`None` の場合は JSON バイト列をそのまま返す。
- §12.1「A publisher MUST NOT use both mechanisms on the same track. If the track property is set, then every object in the track is compressed and the object property MUST NOT be present. If compression varies between objects, then the track property MUST NOT be set and the object property MUST be used on each compressed object.」はトラック単位の制約であり、単一ペイロードを encode する `src/msf.rs` では検証不能である。publisher 側セッション層で強制する。
- publisher 側セッション層は、自側が送出した MSF_COMPRESSION Track Property を保持し、Object 送信時に Track Property が設定されていれば Object Properties に MSF_COMPRESSION が含まれないことを検証する。違反は `ProtocolViolation` として拒否する。
- subscriber 側で peer が Track Property と Object Property の両方を送信した場合 (§12.1 違反入力) は、Track Property を優先して Object Property を無視するのではなく、`MalformedTrack` として subscription を cancel する (session は閉じない)。

### 4. セッション層統合 (publisher / subscriber の MUST 規定)

`src/msf.rs` の修正だけでは §12.1 準拠にならない。property の授受経路をセッション層とアプリ層に設計する:

- **publisher 側**: §12.1.1 に従い、PUBLISH 送信 (`src/session/subscription/send.rs` の `send_publish`) / SUBSCRIBE_OK 送信 (`send_subscribe_ok`) / FETCH_OK 送信 (`src/session/fetch.rs` の `send_fetch_ok`) に渡す `TrackProperties` に MSF_COMPRESSION Track Property を含める。§12.1.2 に従い、圧縮 object ごとに Object Properties に MSF_COMPRESSION Object Property を含める (`src/session/data.rs` の `send_subgroup_object` / `send_object_datagram` / `send_fetch_object_with_context`)。セッション層はこれらを検証し、wire に載せる。
  - `INCLUDE_PROPERTIES=0` が要求された SUBSCRIBE_OK / FETCH_OK でも、MSF_COMPRESSION は §12.1.1 の MUST include を満たすため落とさない (他の Track Properties は空にしてよい)。draft-ietf-moq-transport-23 §9.19.22 は空化を SHOULD とするため、MSF の MUST が MOQT の SHOULD に優先する。
- **subscriber 側**: §12.1.1 に従い、PUBLISH / SUBSCRIBE_OK 受信 (`src/session/subscription/recv.rs`) と FETCH_OK 受信 (`src/session/fetch.rs`) で Track Property を取得し、payload 処理前に確認する。§12.1.2 に従い、受信 object の Object Properties を確認する。
  - セッション層は受信した Track Properties を `Subscription::peer_track_properties` / `Fetch::track_properties` に保持し、Object の properties bytes を受信 Object に保持する。subscriber 側のアプリ層 (crates/moq-sub) はこれらから `MsfCompression` を得て、`src/msf.rs` の decode に渡す。publisher 側のアプリ層 (crates/moq-pub) は `MsfCompression` を encode に渡す。
  - 新しい `SessionEvent` は必須ではない。既存の `Subscription::peer_track_properties` / `Fetch::track_properties` / 受信 Object の properties bytes を使う。

### 5. magic byte 検出ロジックの扱い

**draft-02 strict 準拠を採用する**。根拠: CODEBASE.md「最新ドラフトに準拠すること」、および §12.1.1「If the property is absent, and no per-object compression is signaled, the subscriber MUST treat the payload as uncompressed.」。property 無しで gzip バイトを受信したとき magic byte 検出で展開する案 A (旧 draft 互換 fallback) は「uncompressed として扱う」規定に違反するため採用しない。

draft-02 strict の結果、既存の magic byte 検出 (`is_gzip_compressed` / `maybe_decompress_gzip` の自動判定) は、signal に基づく明示的な圧縮判定に置き換える。
これに伴い、`TimelineEncodingOptions` を使う既存テスト・PBT・fuzz は `MsfCompression` を渡す形に移行する。少なくとも次を更新する:

- `tests/test_msf/timeline_gzip.rs` の `decode_rejects_corrupted_gzip` / `decode_rejects_truncated_gzip` / `decode_rejects_gzip_bomb` / `decode_empty_buffer_as_raw_json_fails` / `media_timeline_roundtrip_gzip_false` / `event_timeline_roundtrip_gzip_false` / `gzip_is_smaller_for_many_entries` / `encode_event_timeline_gzip_propagates_data_raw_error`
- `tests/test_msf/media_timeline.rs` の `TimelineEncodingOptions` 使用箇所
- `pbt/tests/prop_msf.rs` の `media_timeline_gzip_roundtrip` / `event_timeline_gzip_roundtrip`
- `fuzz/fuzz_targets/fuzz_decode_msf_timeline_gzip.rs`
- `tests/test_msf.rs` / `pbt/tests/prop_msf.rs` の import
- `MsfCatalogDocument::encode` / `decode` の全呼び出し側

### 6. 未サポート Value の扱い (§12.1.1 と §12.1.2 で挙動が異なる)

- **Track Property (§12.1.1)**: 「If the property is present with a value the subscriber does not support, the subscriber MUST NOT attempt to process the payload and SHOULD unsubscribe from the track」。未サポート Value 受信時は payload を処理せず、呼び出し側に track unsubscribe を促す `MessageError::UnsupportedCompression(value)` を返す。この variant は `session_termination_code` で `None` を返し、session を閉じない。
- **Object Property (§12.1.2)**: 「If the property is present with a value the subscriber does not support, the subscriber MUST NOT attempt to process that object」。当該 object のみ処理せず (skip/reject)、track は継続購読する (unsubscribe は規定されていない)。Track Property 版とは別に object 単位の処理経路とテストを設ける。
- 両規定とも「未サポート」は値 2 以上で生じる (0 / 1 は MUST support、§12.1)。

### 7. IANA registry 拡張性

§14.4 の "MSF Compression Algorithms" registry (Table 16) は IANA 管理で、値 2-127 は Standards Action、128 以上は private use。
将来 algorithm が追加されうる。
`MsfCompression::try_from(u64)` が 2 以上でエラーを返す設計 (「2. データモデル」参照) により、未知値は §12.1.1 / §12.1.2 の MUST NOT process 経路に乗る。
private use 値 (128 以上) の相互合意サポートは仕様が想定するが、本 issue では 0 / 1 のみサポートし 2 以上は未サポートとして扱う。

## 完了条件

- `src/msf.rs` に `MsfCompression` enum (`None` (0) / `Gzip` (1)、`try_from(u64)` は 2 以上で `MessageError::UnsupportedCompression(value)`) が定義されている。
- `MessageError::UnsupportedCompression(u64)` が追加され、`Display` / `reason` / `session_termination_code` (`None` を返す) など既存の match が更新されている。
- `src/track_properties.rs` に `PROP_MSF_COMPRESSION = 0x78` 定数と `TrackProperties::msf_compression() -> Option<u64>` アクセサが追加されている。
- `src/object_properties.rs` に `ObjectProperties::msf_compression() -> Option<u64>` アクセサが追加されている。
- `src/property_scope.rs` の `known_property_scope` に 0x78 が `PropertyScope::Both` として登録され、`TrackProperties::validate` / `ObjectProperties::validate` のスコープ検証と多重検証が 0x78 に適用される。0x78 のスコープと多重拒否を固定するテストが追加されている。
- `src/msf.rs` の `TimelineEncodingOptions` が廃止され、`encode_media_timeline` / `decode_media_timeline` / `encode_event_timeline` / `decode_event_timeline` が `MsfCompression` を引数で受け取り、signal に基づいて圧縮可否を判定する。
- `MsfCatalogDocument::encode` / `decode` が `MsfCompression` を引数で受け取り、signal に基づいて gzip 圧縮・復号する。全呼び出し側が更新されている。
- signal なし (`MsfCompression::None`) で gzip バイトを decode した場合は展開せず、JSON パースエラーになる。signal が `Gzip` の場合は magic byte を再検出せず必ず展開し、復号できなければ `GzipDecode` を返す。
- publisher 側で PUBLISH / SUBSCRIBE_OK / FETCH_OK に MSF_COMPRESSION Track Property を載せられ、圧縮 object ごとに MSF_COMPRESSION Object Property を載せられる。セッション層が自側の MSF_COMPRESSION Track Property を保持し、Track Property 設定時に Object Property が含まれる送信を `ProtocolViolation` で拒否する。
- `INCLUDE_PROPERTIES=0` が要求された SUBSCRIBE_OK / FETCH_OK でも MSF_COMPRESSION Track Property が保持される (他の Track Properties は空にできる)。
- subscriber 側で PUBLISH / SUBSCRIBE_OK / FETCH_OK の Track Property と受信 object の Object Properties を確認でき、payload 処理前に `MsfCompression` を得て `src/msf.rs` の decode に渡せる。
- peer が Track Property と Object Property の両方を送信した場合、subscriber 側は `MalformedTrack` として subscription を cancel する (session は閉じない)。
- 未サポート Value (2 以上) 受信時: Track Property は payload 未処理 + track unsubscribe を促す `MessageError::UnsupportedCompression`、Object Property は当該 object のみ未処理 (track 継続) が実装されている。
- §5.5 Catalog Compression (catalog 自身の圧縮 signal、catalog track の MOQT property として signal) が library の `MsfCatalogDocument::encode` / `decode` とテストで動作する。
- magic byte 検出は draft-02 strict で signal ベースに置き換え済み。既存の signal なし gzip テスト・PBT・fuzz は `MsfCompression` を渡す形に移行済み。
- 新規テスト追加:
  - Track Property 指定時の Catalog encode/decode と全 Object の gzip 圧縮・復号
  - Object Property 指定時の個別圧縮 (catalog 内で圧縮済み / 未圧縮 Object が混在するケース、draft-02 §12.1.2 で示される例)
  - Track Property と Object Property の同時設定が publisher 側で `ProtocolViolation` になり、subscriber 側で `MalformedTrack` になる
  - 未サポート Value 受信時: Track Property 版 (SHOULD unsubscribe) と Object Property 版 (当該 object のみ処理せず track 継続) の両方
  - §5.5 Catalog 自身の圧縮
  - `INCLUDE_PROPERTIES=0` の SUBSCRIBE_OK / FETCH_OK で MSF_COMPRESSION が保持される
- `tests/test_msf/` / `pbt/tests/prop_msf.rs` に対応テストが追加されている。
- decode 系関数のシグネチャ変更に伴い、fuzz target (`fuzz/fuzz_targets/fuzz_decode_msf_timeline_gzip.rs`、`fuzz_decode_msf_catalog.rs`、`fuzz_roundtrip.rs`、`fuzz_msf_apply_delta.rs` 等) と `crates/moq-pub` / `crates/moq-sub` の呼び出し側を更新する (`Cargo.toml` の `[workspace]` の `exclude = ["fuzz"]` により workspace コマンドでは検出されないため、fuzz ディレクトリで別途 `cargo build` を確認する)。
- `docs/msf.md` の gzip 圧縮・自動展開の記述を `MsfCompression` ベースに更新する。
- `docs/missing-features.md` の MSF_COMPRESSION の TBD 記述を削除し、issue パス参照を外す。
- `src/msf.rs` / `src/error.rs` / `Cargo.toml` の Event Timeline data format の §8.1 参照を §8.2 に更新する (Event Timeline header data の §8.1 はそのまま)。
- Property Type 0x78 が application-specific range (0x78-0x7F) に属し IANA 登録不要であること、および未サポート値 (2 以上) の扱いがコードコメントに明記されている。
- `cargo fmt --all -- --check` / `cargo build --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` が通る。

## 解決方法

(実装後に記載)
