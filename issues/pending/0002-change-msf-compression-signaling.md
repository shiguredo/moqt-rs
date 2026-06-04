# msf の Timeline gzip 圧縮を MSF_COMPRESSION property signaling 機構に追従させる

- Priority: Medium
- Created: 2026-06-30
- Model: Opus 4.7
- Branch: feature/change-msf-compression-signaling
- Polished: 2026-07-22
- Updated: 2026-09-09

## pending にした理由

MSF_COMPRESSION の Track Property ID が IANA 未割当 (Table 14 で TBD) であり、Object Property については §14.4 に Object Properties レジストリへの登録要求自体が存在しない。wire 上の encode/decode とセッション層の MUST include / MUST check を完了条件どおり実装する根拠が仕様側に無いため、今着手しても完了条件を誠実に満たせない。

案 (a)（wire をスコープ外にし値型・codec API・セッション層の受け渡し設計だけ進める）も検討したが、PUBLISH / SUBSCRIBE_OK / Object への property 付与がスタブ化し、一方で magic byte 検出の廃止（設計方針 5 の案 B）だけを先に入れると -00 互換も -01 相互運用も取れない穴が空く。IANA 割当（および Object Property ID の仕様上の確定）を待つのが妥当と判断した。

着手時に確定済みの方針（reopened 後も踏襲する）:

- Track Property ID / Object Property ID が仕様で確定するまで実装しない（暫定 ID による wire 実装は採用しない）。
- magic byte 検出は案 B (-01 strict) を採用する（property 無しは uncompressed として扱う）。
- peer が Track Property と Object Property を同一 track で両方送ってきた場合は protocol violation として拒否する。

IANA 割当と Object Property ID の仕様確定後に、本 issue を `issues/` へ戻して対応する。

## 目的

draft-ietf-moq-msf-01 で導入された MSF_COMPRESSION property による compression signaling 機構に追従する。現実装は gzip magic byte 検出による implicit な圧縮判定で、-01 仕様の Track Property / Object Property を一切参照していない。-01 peer との相互運用で signaling 不整合が生じうるため挙動追従が必要。

動機:

- -01 §7.1 / §8.1 は payload を `MAY be compressed using the MSF_COMPRESSION property (Section 12.1)` と規定。
- -01 §12.1 (Compression Signaling) で Track Property と Object Property の 2 つの mutually exclusive な signaling mechanism が定義された。
- §12.1.1 は `Publishers MUST include the MSF_COMPRESSION track property` と MUST 規定。
- §5.5 (Catalog Compression) で catalog 自身も §12.1 に基づく圧縮 signal の対象になった。

## 優先度根拠

Medium。-01 仕様準拠と peer との相互運用性に影響するため Low ではない。一方、現実装の magic byte 検出は -01 の property signaling に追従していない (設計方針 5 で案 B に置き換える)。gzip バイトを無条件に展開するだけで、緊急の堅牢性問題ではない。

## 現状

- `src/msf.rs` の Timeline gzip 圧縮・復号関連
  - `TimelineEncodingOptions`: `pub struct { pub gzip: bool }` - MSF_COMPRESSION property に紐付かない bool フラグ
  - `is_gzip_compressed`: gzip magic byte `0x1F 0x8B` 検出のみ
  - `maybe_decompress_gzip`: magic byte 検出による自動展開 (ストリーミング展開、上限 16 MiB の防御は実装済み)
  - `maybe_compress_gzip`: `TimelineEncodingOptions.gzip` で圧縮可否を判定
  - `encode_media_timeline` / `encode_event_timeline` 系: 圧縮判定は引数の `TimelineEncodingOptions` のみで signal していない
- MOQT 層の `src/track_properties.rs` (`TrackProperties`) に MSF_COMPRESSION Track Property (§12.1.1) の定数・アクセサが無い。
  `src/object_properties.rs` (`ObjectProperties`) に MSF_COMPRESSION Object Property (§12.1.2) のアクセサが無い。
  MSF_COMPRESSION は MOQT 層 property であり (§12)、`src/msf.rs` の JSON モデル (`MsfTrack` / `MsfCloneTrack` / `MsfCatalog` / 各 Entry) へのフィールド追加は対象外。
- セッション層 (`src/session/subscription/recv.rs` / `send.rs` / `src/session/data.rs`) で §12.1.1 / §12.1.2 の Track/Object Property の授受 (publisher 側 MUST include / subscriber 側 MUST check) を扱っていない。
- §5.5 (Catalog Compression) の catalog 自身の圧縮 signal (catalog track の MOQT property として signal) を扱っていない。

## 設計方針

### 0. 前提: MSF_COMPRESSION property の輸送レイヤ

draft-ietf-moq-msf-01 §12 は「These properties are carried in MOQT control messages and object headers, allowing endpoints to learn track and object characteristics before processing payload data」と規定する。
MSF_COMPRESSION は **MOQT 層の Track Property / Object Property** であり、catalog JSON (§5.1 / §5.2) や timeline JSON (§7.1 / §8.1) のメンバとして定義されたフィールドは存在しない。
したがって本 issue の統合点は `src/msf.rs` の JSON モデル (`MsfTrack` / `MsfCloneTrack` / `MsfCatalog` / 各 Entry) ではなく、MOQT 層の `src/track_properties.rs` (`TrackProperties`、PUBLISH / SUBSCRIBE_OK / FETCH_OK で使用) と `src/object_properties.rs` (`ObjectProperties` / `ObjectPropertyTracker`) である。
`src/msf.rs` の codec 関数 (`encode_media_timeline` 等) は圧縮 signal を引数で受け取る純粋関数のままにし、JSON モデルにはフィールドを追加しない。

### 1. 設計上の制約: Track Property ID が IANA 未割当 (TBD)

Table 14 の `MSF_COMPRESSION` Track Property の Property ID は **TBD** (IANA 未割当、Table 14 には枠がある) である。
一方 Object Property は §14.4 に Object Properties レジストリへの登録要求自体が存在しない (§14.3 の Track Property のような "register the following entry" 文がなく、ID の枠すらない)。
§14.4 が創設するのは "MSF Compression Algorithms" **値レジストリ** (Table 15) のみである。
draft-ietf-moq-transport-20 §1.4.3 (Key-Value-Pair Structure) の KVP 規則 (Figure 2) では varint 値は偶数型 (`TrackProperties::encode` の偶数型検証参照) であり、割当 ID は偶数でなければならないが、TBD では確定できない。

これは wire 上の encode/decode を実装・相互運用するための Property ID が仕様上に存在しないことを意味し、本 issue の着手時に以下の方針を確定する:

- **案 (a) wire encode/decode をスコープ外とする**: IANA 割当まで MOQT wire 上の Property encode/decode は実装せず、本 issue は (i) `MsfCompression` 値型 (Table 11 / Table 15 に基づく `None` (0) / `Gzip` (1)) の定義、(ii) `src/msf.rs` codec 層の signal 受け渡し API (`TimelineEncodingOptions.gzip` 廃止、signal 引数化)、(iii) セッション層の signal 受け渡し経路の設計、に限定する。
- **案 (b) 暫定 ID を仮定する**: 暫定の Property ID を仮定して wire encode/decode を実装し、「IANA 割当まで他実装と相互運用不能」であることをコードコメントと完了条件に明記する。

いずれを採るにしても TBD 状態と方針を完了条件に反映する (本 polish では案の確定は行わず、着手時の判断項目として明示する)。

### 2. データモデルへの MSF_COMPRESSION 値型追加

- compression algorithm の値型を Table 11 / Table 15 に基づき `MsfCompression` enum として定義する: `None` (0) / `Gzip` (1)。§12.1「All MSF implementations MUST support both uncompressed payloads (value 0 or property absent) and GZIP compressed payloads (value 1)」により 0 と 1 は全実装が MUST support。
- 未知値 (2 以上) の扱い: Table 15 で値 2-127 は Standards Action、128 以上は private use。`MsfCompression::try_from(u64)` は 2 以上でエラーを返し、§12.1.1 / §12.1.2 の「MUST NOT attempt to process」を処理境界で保証する (未知値を `Unknown(u64)` として上流に流して処理を継続するのは MUST NOT 違反になるため採用しない)。
- `src/track_properties.rs` に `PROP_MSF_COMPRESSION` 定数 (ID は案 (a)/(b) の確定後) と `TrackProperties::msf_compression()` アクセサ (`TrackProperties::dynamic_groups` と同型) を追加する。
- `src/object_properties.rs` に同様の MSF_COMPRESSION Object Property アクセサを追加する。
- `src/msf.rs` の JSON モデル (`MsfTrack` / `MsfCloneTrack` / `MsfCatalog` / `MsfMediaTimelineEntry` / `MsfEventTimelineEntry`) にはフィールドを追加しない (MSF_COMPRESSION は MOQT 層 property のため)。

### 3. encode/decode 経路の刷新

- `src/msf.rs` の encode 系関数 (`encode_media_timeline` / `encode_event_timeline` 等) のシグネチャから `TimelineEncodingOptions.gzip` 単独制御を廃し、`MsfCompression` signal を引数で受け取る。
- `maybe_decompress_gzip` の呼び出しは signal に基づいて圧縮可否を判定する (magic byte 検出の扱いは「5. magic byte 検出ロジックの扱い」参照)。
- §12.1「A publisher MUST NOT use both mechanisms on the same track. If the track property is set, ... the object property MUST NOT be present」はトラック単位の制約であり、単一ペイロードを encode する `src/msf.rs` では検証不能。publisher 側セッション層で強制する (Track Property 設定時は Object Property を付与しない validation)。
- decode 時に peer が Track Property と Object Property の両方を送信した場合 (§12.1 違反入力) の扱いを設計する: §12.1 は Track Property 設定時に Object Property の MUST NOT be present を規定するため、subscriber 側はこれを protocol violation として拒否するか、Track Property を優先して Object Property を無視するかを着手時に確定する。

### 4. セッション層統合 (publisher / subscriber の MUST 規定)

`src/msf.rs` の修正だけでは §12.1 準拠にならない。property の授受経路をセッション層に設計する:

- **publisher 側**: §12.1.1「Publishers MUST include the MSF_COMPRESSION track property in the PUBLISH message (publisher-initiated flow) or SUBSCRIBE_OK (subscriber-initiated flow)」に従い、PUBLISH 送信 (`src/session/subscription/send.rs` の `send_publish`) / SUBSCRIBE_OK 送信時に Track Property を付与する。
  §12.1.2「Publishers MUST include the MSF_COMPRESSION object property on each compressed object」に従い、圧縮 object ごとに Object Property を付与する (`src/session/data.rs` の Object 送信経路)。
- **subscriber 側**: §12.1.1「Subscribers MUST check for this property in the corresponding message before processing the track payload」に従い、PUBLISH / SUBSCRIBE_OK 受信 (`src/session/subscription/recv.rs`) で Track Property を取得し、payload 処理前に `src/msf.rs` の decode に渡す。
  §12.1.2「Subscribers MUST check for this property on each received object before processing its payload」に従い、object ごとに Object Property をチェックする (`src/session/data.rs` の `ObjectProperties::decode` 経路)。
- signal の露出方法 (`SessionEvent` への追加、decode API への引数追加等) は着手時に確定する。

### 5. magic byte 検出ロジックの扱い

**案 B (-01 strict 準拠) を採用する**。根拠: CODEBASE.md「最新ドラフトに準拠すること」、および §12.1.1「If the property is absent, and no per-object compression is signaled, the subscriber MUST treat the payload as uncompressed」。property 無しで gzip バイトを受信したとき magic byte 検出で展開する案 A (-00 互換 fallback) は「uncompressed として扱う」規定に違反するため採用しない。

案 B の結果、既存の magic byte 検出 (`is_gzip_compressed` / `maybe_decompress_gzip` の自動判定) は、signal に基づく明示的な圧縮判定に置き換える。
これに伴い、signal なしで gzip バイトを decode することを前提とした既存テスト (`tests/test_msf/timeline_gzip.rs` の `decode_rejects_corrupted_gzip` / `decode_rejects_truncated_gzip` / `decode_rejects_gzip_bomb`、
`pbt/tests/prop_msf.rs` の `media_timeline_gzip_roundtrip` / `event_timeline_gzip_roundtrip`) は「Gzip signal 付きで decode」するよう移行する (回帰ではなく意図的な挙動変更。完了条件で移行対象テストを明示する)。

### 6. 未サポート Value の扱い (§12.1.1 と §12.1.2 で挙動が異なる)

- **Track Property (§12.1.1)**: 「If the property is present with a value the subscriber does not support, the subscriber MUST NOT attempt to process the payload and SHOULD unsubscribe from the track」。未サポート Value 受信時は payload を処理せず、呼び出し側に track unsubscribe を促す `MessageError` バリアントを返す。
- **Object Property (§12.1.2)**: 「If the property is present with a value the subscriber does not support, the subscriber MUST NOT attempt to process that object」。当該 object のみ処理せず (skip/reject)、track は継続購読する (unsubscribe は規定されていない)。Track Property 版とは別に object 単位の処理経路とテストを設ける。
- 両規定とも「未サポート」は値 2 以上で生じる (0 / 1 は MUST support、§12.1)。

### 7. IANA registry 拡張性

Table 15 の "MSF Compression Values" は IANA 管理で、値 2-127 は Standards Action、128 以上は private use。
将来 algorithm が追加されうる。
`MsfCompression::try_from(u64)` が 2 以上でエラーを返す設計 (「2. データモデル」参照) により、未知値は §12.1.1 / §12.1.2 の MUST NOT process 経路に乗る。
private use 値 (128 以上) の相互合意サポートは仕様が想定するが、本 issue では 0 / 1 のみサポートし 2 以上は未サポートとして扱う。

## 完了条件

- `MsfCompression` enum (`None` (0) / `Gzip` (1)、`try_from(u64)` は 2 以上でエラー) が定義されている。
- `src/track_properties.rs` に `PROP_MSF_COMPRESSION` 定数と `TrackProperties::msf_compression()` アクセサが追加されている (Property ID は案 (a)/(b) の確定結果に従う)。
- `src/object_properties.rs` に MSF_COMPRESSION Object Property アクセサが追加されている。
- `src/msf.rs` の encode/decode 系関数が `TimelineEncodingOptions.gzip` 単独制御ではなく `MsfCompression` signal を引数で受け取り、signal に基づいて圧縮可否を判定する。
- セッション層が publisher 側で PUBLISH / SUBSCRIBE_OK に Track Property を付与し、圧縮 object ごとに Object Property を付与する。subscriber 側で payload 処理前に Track/Object Property をチェックし `src/msf.rs` decode に渡す。
- Track Property と Object Property の同時設定が publisher 側で validation され (§12.1 MUST NOT)、peer からの違反入力 (両方受信) の扱いが確定・実装されている。
- 未サポート Value (2 以上) 受信時: Track Property は payload 未処理 + track unsubscribe を促す `MessageError`、Object Property は当該 object のみ未処理 (track 継続) が実装されている。
- §5.5 Catalog Compression (catalog 自身の圧縮 signal、catalog track の MOQT property として signal) が動作する。
- magic byte 検出は案 B (strict) で signal ベースに置き換え済み。
  既存の signal なし gzip テスト (`tests/test_msf/timeline_gzip.rs` の `decode_rejects_corrupted_gzip` / `decode_rejects_truncated_gzip` / `decode_rejects_gzip_bomb`、`pbt/tests/prop_msf.rs` の `media_timeline_gzip_roundtrip` / `event_timeline_gzip_roundtrip`) は Gzip signal 付き decode に移行済み。
- 新規テスト追加:
  - Track Property 指定時の Catalog encode/decode と全 Object の gzip 圧縮・復号
  - Object Property 指定時の個別圧縮 (catalog 内で圧縮済み / 未圧縮 Object が混在するケース、draft-ietf-moq-msf-01 §12.1.2 で示される例)
  - Track Property と Object Property の同時設定が MUST NOT として拒否される
  - 未サポート Value 受信時: Track Property 版 (SHOULD unsubscribe) と Object Property 版 (当該 object のみ処理せず track 継続) の両方
  - §5.5 Catalog 自身の圧縮
- `tests/test_msf/` / `pbt/tests/prop_msf.rs` に対応テストが追加されている。
- decode 系関数のシグネチャ変更は fuzz target (`fuzz/fuzz_targets/fuzz_decode_msf_timeline_gzip.rs` 等) も更新する (`Cargo.toml` の `[workspace]` の `exclude = ["fuzz"]` により workspace コマンドでは検出されないため、fuzz ディレクトリで別途 `cargo build` を確認する)。
- Track Property ID の TBD 状態と採用方針 (案 (a) スコープ外 / 案 (b) 暫定 ID) がコードコメントと commit メッセージに明記されている。
- `cargo fmt --check --all` / `cargo build --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` が通る。

## 解決方法

(実装後に記載)
