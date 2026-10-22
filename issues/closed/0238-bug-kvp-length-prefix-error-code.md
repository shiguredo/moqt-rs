# Message Parameters と SETUP Options の宣言長超過を KEY_VALUE_FORMATTING_ERROR で閉じる

- Created: 2026-10-12
- Completed: 2026-10-23
- Branch: feature/fix-kvp-length-prefix-error-code
- Polished: 2026-10-23

## 目的

draft-ietf-moq-transport-22 §8.3 (Key-Value-Pair Structure) は "If a receiver understands a
Type, and the following Value or Length/Value does not match the serialization defined by that
Type, the receiver MUST close the session with error code KEY_VALUE_FORMATTING_ERROR." と定める。
型を理解したうえで長さプレフィックスが値と整合しない場合に PROTOCOL_VIOLATION で閉じており
MUST を満たしていない。本 issue の対象は Message Parameters と SETUP Options が持つ KVP の
Length (型が奇数のときの値長) に限る。

## 現状

- `src/message_parameter.rs` の `MessageParameters::decode_inner` は各パラメータ値を
  `decode_value` でデコードする。`ValueEncoding::LengthPrefixed`
  (AUTHORIZATION_TOKEN / FILL_PARAMETERS / SUBGROUP_FILTER / OBJECTID_FILTER /
  PRIORITY_FILTER / OBJECT_PROPERTY_FILTER / TRACK_PROPERTY_FILTER) は Length varint の
  デコード後に `varint::checked_len` を呼び、宣言長が残りバッファを超えると
  `MessageError::UnexpectedEof` を返す。`decode_fill_parameters` も同じ `decode_inner` を
  共有するため、FILL_PARAMETERS の内側の KVP も同じ挙動になる。
- `src/parameter.rs` の `SetupOptions::decode` は奇数型の値長を同じく `varint::checked_len` で
  検証し、宣言長が残りバッファを超えると `UnexpectedEof` を返す。
- 写像は次の 2 経路がある。
  - `examples/tokio-moq/src/error.rs` の `session_error_code` は `UnexpectedEof` を
    PROTOCOL_VIOLATION (0x3) に写す。
  - library の control plane では `src/session/core.rs` の `session_error_from_request_body` が
    `KeyValueFormattingError` を `SESSION_KEY_VALUE_FORMATTING_ERROR` (0x6)、
    `ProtocolViolation` を 0x3 に写す。未対応 request の body は
    `Session::handle_unsupported_request` が `decode_unsupported_request_body` で解析するため、
    `MessageParameters::decode` の失敗はこの写像を通る。`session_error_from_request_body` の
    doc は「値の長さプレフィックスが残りバッファを超える場合は codec が `UnexpectedEof` を
    返すため `PROTOCOL_VIOLATION` に写る」と現行挙動を明記しており、修正後は虚偽になる。
  - そのため現行は、Message Parameters の長さ不整合が 0x3、SETUP Options の長さ不整合も
    example の写像で 0x3 になる。
- 宣言長が 2^16-1 を超える場合は `ProtocolViolation` で、§8.3 が MUST とする
  PROTOCOL_VIOLATION (0x3) と一致している (こちらは正しい)。
- 既存テストが現行挙動を固定している。修正すると次の 2 件が必ず失敗するため更新が要る。
  - `tests/test_message_parameter.rs` の `unexpected_eof_in_length_prefixed`
    (`count=1`, AUTHORIZATION_TOKEN, 宣言長 10, 残り 3 バイトのワイヤに `UnexpectedEof` を要求)
  - `tests/test_parameter.rs` の `unexpected_eof_in_value` (同じ形の Setup Options に
    `UnexpectedEof` を要求)
- wire 経路の切り詰めとの区別: `src/decoder.rs` の `MessageDecoder::try_decode_message` は
  Type + Length + payload が揃うまで `Ok(None)` を返し、`src/message.rs` の
  `ControlMessage::decode` も payload 長を本体 decode より先に検証する。そのため「メッセージ
  frame が途中で切れている」場合は `UnexpectedEof` になり (`tests/test_message.rs` の
  `payload_shorter_than_length` が固定)、本 issue が変更する「frame は揃っていて KVP の
  宣言長だけが残りを超える」場合とは層で区別される。`MessageParameters::decode` /
  `SetupOptions::decode` は完全なスコープ (メッセージ payload の残り全体 / FILL_PARAMETERS の
  値全体) を受け取る契約であり、部分バッファを渡す公開 API 利用者にはこの契約を doc に明記する
  (挙動は変えない)。

## 設計方針

- `src/message_parameter.rs` の `decode_value` の `ValueEncoding::LengthPrefixed` で、
  宣言長が残りバッファを超える場合は `MessageError::KeyValueFormattingError` を返す。
  宣言長が 2^16-1 を超える場合の `ProtocolViolation` は変えない。`varint::checked_len` は
  他の用途と共有しているため、関数の意味は変えず呼び出し側で文脈に応じて写す。
- `src/parameter.rs` の `SetupOptions::decode` の奇数型分岐も、宣言長が残りバッファを超える
  場合は `KeyValueFormattingError` を返す。偶数型 (varint 値) の途中終端は `UnexpectedEof` の
  ままとする。
- 残す `UnexpectedEof` は「フレーミング自体が読めない終端」に限る。具体的には count / delta
  key / Type varint の途中終端、Length varint の途中終端、偶数型の値 varint の途中終端、
  LOCATION_FILTER の必須フィールド欠落、FILL_PARAMETERS 内側の count / varint 終端である。
  これらは「Type を理解したうえで Length/Value が定義と一致しない」状態ではない。
  なお LOCATION_FILTER は Length ではなく Value の不整合であり、本 issue の対象外とする
  (挙動は変えない)。
- FILL_PARAMETERS の内側の長さプレフィックス付き値が内側スコープの残りを超える場合も、
  同じ `decode_value` を通るため `KeyValueFormattingError` になる。`src/message_parameter.rs` の
  `decode_fill_parameters` の doc にある「値が途中で切れた場合は外側と同じ分類
  (`UnexpectedEof` / `ProtocolViolation`) のまま」を、varint 終端は `UnexpectedEof` のまま・
  Length 超過は `KeyValueFormattingError` に更新する。
- 写像コード (`session_error_from_request_body` / `session_error_code`) は変更しない。
  既存の写像で `KeyValueFormattingError` が 0x6 に写るため MUST を満たす。ただし
  `src/session/core.rs` の `session_error_from_request_body` の doc と
  `decode_unsupported_request_body` の `# Errors` を修正後の挙動に合わせて更新する。
- 次の doc を更新する。
  - `src/message_parameter.rs` の `MessageParameters::decode` の `# Errors`: Length 超過を
    `KeyValueFormattingError` に分類し、残る `UnexpectedEof` の範囲 (上記) を列挙する。
    完全なスコープを受け取る契約も本文に書く。
  - `src/parameter.rs` の `SetupOptions::decode`: `# Errors` 節を新設し、同じ分類を書く。
  - `src/message_parameter.rs` の `decode_fill_parameters` の doc。
  - `src/session/core.rs` の `session_error_from_request_body` の doc と
    `decode_unsupported_request_body` の `# Errors`。
- 次のテストを更新する。
  - `tests/test_message_parameter.rs` の `unexpected_eof_in_length_prefixed` を
    `KeyValueFormattingError` 期待に変更し、テスト名・コメントも実態に合わせる。
  - `tests/test_parameter.rs` の `unexpected_eof_in_value` を同様に変更する。
  - `tests/test_message_parameter.rs` の `truncated_inner_value_is_unexpected_eof` は varint
    途中切れを固定しており、変更不要であることを確認する。
- 対象外: Properties 系 (Track / Object / LOC Properties) の KVP Length。理由は次のとおり。
  - 外側の Properties Length は §8.4 / §11.1.2 の framing であり §8.3 の Length ではない。
  - `src/object_properties.rs` の `ObjectProperties::decode_exact` は `decode` の**全エラー**を
    `ProtocolViolation` に畳む設計で、data plane の失敗は session close ではなく該当
    subscription の終端になる (`src/session/data.rs` の `terminate_malformed_track`、
    `tests/test_session/data_stream.rs` が固定)。この畳み込みの変更は別 issue とする。
  - `src/track_properties.rs` の `decode_track_kv_pairs` は内側 KVP の Length 超過を現行どおり
    `UnexpectedEof` (0x3) で返す。Track Properties は外側 Length を持たず `decode_exact` も
    通らないため上記の除外理由が当てはまらないが、同じ §8.3 の KVP Length でありながら本 issue
    では対象外とする。分類の変更は未起票の別 issue で扱う (0311 は Properties の variant 統一
    を扱うが、この decode 側の Length 超過分類は対象外である)。
- 本 issue の変更は codec 層 2 箇所と doc・テストに限る。写像と公開 API の形は変えない。

## 完了条件

- codec: `MessageParameters::decode` が、宣言長が残りバッファを超える KVP を含む完全なスコープ
  に対して `MessageError::KeyValueFormattingError` を返すこと。Length varint の途中終端は
  `UnexpectedEof` のままであること (`tests/test_message_parameter.rs`)。
- codec: `SetupOptions::decode` が同じ条件で `KeyValueFormattingError` を返すこと。Type /
  Length varint の途中終端は `UnexpectedEof` のままであること (`tests/test_parameter.rs`)。
- codec: FILL_PARAMETERS の内側の KVP で宣言長が内側スコープの残りを超える場合に
  `KeyValueFormattingError`、varint 終端は `UnexpectedEof` であること
  (`tests/test_message_parameter.rs`)。
- wire メッセージ: payload は揃っていて KVP の宣言長だけが残りを超える SUBSCRIBE のワイヤを
  `ControlMessage::decode` に渡すと `KeyValueFormattingError` になること。payload 長より実
  データが短いワイヤは従来どおり `UnexpectedEof` であること (`tests/test_message.rs`)。
- session: `ControlMessage::Unsupported` の body にそのようなパラメータブロックを持つ未対応
  request を渡すと、session が `SESSION_KEY_VALUE_FORMATTING_ERROR` (0x6) で閉じること
  (`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session`
  の隣)。
- example: `examples/tokio-moq/src/peer_harness.rs` のテストで、初回 SETUP の Setup Options に
  宣言長超過の KVP を載せると client が CONNECTION_CLOSE 0x6 を送ること。0240 が整備した
  終了コード送信経路を使う。確立後の control stream に SETUP を送る経路は現行ハーネスに無く、
  `pump_once` での終了コード送信は 0245 の範囲であるため本 issue では扱わない
  (`session_error_code` が `KeyValueFormattingError` を 0x6 に写すことは既存テストで固定済み)。
- 宣言長が 2^16-1 を超える場合は codec が `ProtocolViolation` を返し、session 経由では 0x3 で
  閉じること (既存挙動の維持)。
- doc: `MessageParameters::decode` / `SetupOptions::decode` / `decode_fill_parameters` /
  `session_error_from_request_body` / `decode_unsupported_request_body` の doc が実装と
  一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

KVP の宣言長が残りバッファを超える場合の分類を draft-ietf-moq-transport-22 §8.3
(Key-Value-Pair Structure) の MUST に合わせ、Message Parameters と SETUP Options の 2 経路で
KEY_VALUE_FORMATTING_ERROR にした。

- `src/message_parameter.rs` の `decode_value` の `ValueEncoding::LengthPrefixed` で、Length
  varint は読めるが宣言長が残りバッファを超える場合に `MessageError::KeyValueFormattingError`
  を返すようにした。宣言長が 2^16-1 を超える場合の `ProtocolViolation` は維持した。
  `varint::checked_len` の意味は変えず、呼び出し側で文脈に応じた variant へ写している。
- `src/parameter.rs` の `SetupOptions::decode` の奇数型分岐も同じく
  `KeyValueFormattingError` を返すようにした。
- FILL_PARAMETERS の内側の KVP も同じ `decode_value` を通るため、内側の宣言長超過も
  `KeyValueFormattingError` になる。varint の途中終端 (count / delta key / Type / Length /
  偶数型の値、LOCATION_FILTER の必須フィールド欠落、TRACK_NAMESPACE_PREFIX の値の途中終端、
  FILL_PARAMETERS 内側の varint) は `UnexpectedEof` のまま維持した。
- 写像コード (`session_error_from_request_body` / `session_error_code`) は変更していない。
  `KeyValueFormattingError` が既存の写像で 0x6 に写るため、未対応 request の body 経由の
  session と example の SETUP 経路で 0x6 で閉じる。
- doc を更新した。`MessageParameters::decode` の `# Errors` と入力契約 (Track Properties が
  後続するメッセージの呼び出し規約と検出限界)、`decode_fill_parameters`、
  `SetupOptions::decode` の `# Errors` 新設、`session_error_from_request_body` の doc、
  `decode_unsupported_request_body` の `# Errors`。
- テストを更新・追加した。`tests/test_message_parameter.rs` の
  `unexpected_eof_in_length_prefixed` と `tests/test_parameter.rs` の
  `unexpected_eof_in_value` を `KeyValueFormattingError` 期待へ更新し、Length / delta key /
  偶数型の値の varint 途中終端、TRACK_NAMESPACE_PREFIX の値の途中終端、FILL_PARAMETERS 内側の
  宣言長超過、wire SUBSCRIBE の decode、未対応 request body 経由の session (0x6、2^16-1 超は
  0x3)、example の初回 SETUP 経由 (0x6) を追加した。
- Properties 系 (Track / Object / LOC Properties) の KVP Length は本 issue の対象外とした。
  `ObjectProperties::decode_exact` は `decode` の全エラーを `ProtocolViolation` に畳み、
  data plane の失敗は subscription 終端になる。`TrackProperties::decode` の内側 KVP の宣言長
  超過は現行どおり 0x3 のままで、分類の変更は別 issue とする。
