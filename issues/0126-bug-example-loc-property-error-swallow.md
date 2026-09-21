# subscriber example が LOC の書式違反を握り潰してセッションを閉じない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-loc-property-error-swallow
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure) は、理解している型の Value / Length がその型の直列化と一致しない場合にセッションを閉じる MUST を定める。

> If a receiver understands a Type, and the following Value or Length/Value does not match the serialization defined by that Type, the receiver MUST close the session with error code KEY_VALUE_FORMATTING_ERROR.

`KEY_VALUE_FORMATTING_ERROR` は同 §12.2 (Session Termination Codes) で `0x6` と定義される。

> KEY_VALUE_FORMATTING_ERROR (0x6):  The key-value pair has a formatting error.

library の `src/loc.rs` はこの検証を `validate_known_property` (Audio Level は 8 bit 範囲) と `validate_known_property_len` (Video Frame Marking は 1-4 バイト) で実装し、`MessageError::KeyValueFormattingError` を返す。ところが subscriber example はこの失敗を「プロパティ無し」に潰すため、違反がエラーとして扱われず、誤った PTS と config 無しの再生が続く。

## 現状

- `examples/moqt-subscriber/src/pipeline.rs` の `extract_video_config` は `LocProperties::decode(bytes).ok()?` で失敗を `None` に潰す
- 同じ file の `extract_timestamp_timescale` は `let Ok((props, _)) = LocProperties::decode(bytes) else { return (None, None); }`、`extract_audio_config` は `let Ok((props, _)) = LocProperties::decode(bytes) else { return None; }` で同じく潰す。
  `extract_audio_config` の doc コメントは「LocProperties のデコード失敗は `None` を返し、検証は静かにスキップされる (不正 properties は `extract_timestamp_timescale` と同じ扱い)」と明記している
- 呼び出し側は `decode_video_stream` (映像の Video Config) と `decode_audio_stream` (Audio Config の検証と `audio_pts_us` による PTS 算出)。違反時は Timestamp / Timescale が `None` になって `audio_pts_us` が 0 を返し、Video Config 無しのデコードが続く
- library の session 層はこの違反を検出しない。
  `src/object_properties.rs` の `ObjectPropertyTracker::observe_object` は汎用の `ObjectProperties::decode` を通すだけで LOC 固有の値域・長さ検証を持たず、失敗は `ProtocolViolation` に写して該当 subscription の Malformed Track 終了として扱われる (`src/session/data.rs`)。
  Video Frame Marking の宣言長 5 や Audio Level 256 は KVP 構造としては妥当なため、この経路では検出されない
- library の `src/loc.rs` の `LocProperties::decode` は `MessageError::KeyValueFormattingError` を返す。`src/error.rs` はこれを「受信側は KEY_VALUE_FORMATTING_ERROR (0x6) でセッションを閉じなければならない (MUST)」と定義し、`SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`) を公開定数として持つ
- `LocProperties::encode` も同じ検証を行うため、違反値を持つ properties ブロックは library の encode では生成できない。テストのフィクスチャは `shiguredo_moqt::varint` で生バイト列を組み立てる必要がある (Video Frame Marking の宣言長 5 は Properties Length `0x07` + `0x09 0x05` + 5 バイトの値、Audio Level 256 は Properties Length `0x03` + `0x0C` + vi64 の `0x81 0x00`)
- example の stream task は `DataPlaneHandle` (`examples/moqt-transport/src/moqt_client.rs`) 経由で `Session` を共有している。
  `Session::close` / `MoqtClient::close` は `SessionEvent::CloseSession` を発行し、`examples/moqt-subscriber/src/pipeline.rs` の main ループは `SessionEvent::CloseSession` を受けて終了する。`CloseSession` は `is_notable_event` に含まれるため、stream task 側から閉じても main ループに届く
- `LocProperties::decode` の失敗には `KeyValueFormattingError` のほかに `UnexpectedEof` と `ProtocolViolation` (重複プロパティ ID など) もある。decoder は Properties Length と実データ長の一致しか検証しないため、KVP の内側が途中で切れたブロックはここで初めて検出される

## 設計方針

- `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` の戻り値を「プロパティ無し」と「書式違反」で区別できる形 (`Result<Option<T>, MessageError>` など) にする。`properties_bytes` が `None` の場合と `LocProperties::decode` が失敗した場合を同じ値にしない
- `MessageError::KeyValueFormattingError` を受けたら §8.3 の MUST どおり `SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`) でセッションを閉じる。プロパティが単に無い場合は従来どおり config / Timestamp 無しで再生を続ける
- `KeyValueFormattingError` 以外の decode 失敗 (`UnexpectedEof` / `ProtocolViolation`) も「プロパティ無し」として黙殺せず、プロトコル違反としてセッション終了側に倒す
- セッションを閉じる経路は example の I/O 層に置く。stream task は `DataPlaneHandle` しか持たないため、`DataPlaneHandle` に `0x6` でセッションを閉じるメソッドを追加し、`handle_stream_body` / `decode_video_stream` / `decode_audio_stream` から呼ぶ。main ループは既存の `SessionEvent::CloseSession` 経路で終了する
- library (`src/loc.rs` / `src/session/`) は変更しない。検証は実装済みで、欠けているのは example 側の扱い

## 完了条件

- Video Frame Marking の宣言長 5、Audio Level 256 のいずれかを含む Object Properties を送る peer に対して、subscriber example が `SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`) でセッションを閉じること。次の 2 つを満たすこと
  - `examples/moqt-subscriber/src/pipeline.rs` の `#[cfg(test)] mod tests` に、`extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` が `KeyValueFormattingError` をエラーとして返し、`properties_bytes` が `None` の場合 (プロパティ無し) と区別されることを固定する unit test が追加されていること
  - セッションが閉じることを確認する手順を、issue の完了時に「実機確認の結果」として記録すること。手順は次のとおり。`examples/moqt-publisher/src/pipeline.rs` の `build_video_loc_properties` を一時的に宣言長 5 の Video Frame Marking を出す形に変えて publisher を起動し、subscriber を接続して `Session closed: 0x6 ...` のログが出て main ループが終了することを確認する。確認後は一時改変を戻す
- 正常な Video Frame Marking (1-4 バイト) と Audio Level (8 bit 範囲) を受信した場合、および `properties_bytes` が `None` の場合の既存挙動 (config / Timestamp 無しでも再生を継続する) が変わらないこと
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること
