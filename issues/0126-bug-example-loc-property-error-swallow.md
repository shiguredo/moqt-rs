# subscriber example が LOC の書式違反を握り潰してセッションを閉じない

- Created: 2026-09-21
- Completed: 2026-09-23
- Branch: feature/fix-example-loc-property-error-swallow
- Polished: 2026-09-22

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
- 呼び出し側は `decode_video_stream` (`extract_video_config`) と `decode_audio_stream` (`extract_audio_config` / `extract_timestamp_timescale`)。違反時は Timestamp / Timescale が `None` になって `audio_pts_us` が 0 を返し、Video Config 無しのデコードが続く
- library の session 層はこの違反を検出しない。
  `src/object_properties.rs` の `ObjectPropertyTracker::observe_object` は汎用の `ObjectProperties::decode` を通すだけで LOC 固有の値域・長さ検証を持たず、失敗は `ProtocolViolation` に写して `src/session/data.rs` の `terminate_malformed_track` で該当 subscription の Malformed Track 終了として扱われる。
  Video Frame Marking の宣言長 5 や Audio Level 256 は KVP 構造としては妥当なため、この経路では検出されない
- library の `src/loc.rs` の `LocProperties::decode` は `MessageError::KeyValueFormattingError` を返す。`src/error.rs` はこれを「受信側は KEY_VALUE_FORMATTING_ERROR (0x6) でセッションを閉じなければならない (MUST)」と定義し、`SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`) を公開定数として持つ
- `LocProperties::encode` も同じ検証を行うため、違反値を持つ properties ブロックは library の encode では生成できない。テストのフィクスチャは `shiguredo_moqt::varint` で生バイト列を組み立てる必要がある (Video Frame Marking の宣言長 5 は Properties Length `0x07` + `0x09 0x05` + 5 バイトの値、Audio Level 256 は Properties Length `0x03` + `0x0C` + vi64 の `0x81 0x00`)
- example の stream task は `DataPlaneHandle` (`examples/moqt-transport/src/moqt_client.rs`) 経由で `Session` を共有している。
  `Session::close` は `SessionEvent::CloseSession` を発行し、`examples/moqt-subscriber/src/pipeline.rs` の main ループは `SessionEvent::CloseSession` を受けて終了する。
  `CloseSession` は `is_notable_event` に含まれ、main ループの `MoqtClient::next_event` → `take_notable_event` が `Session::poll_event` を直接 poll するため、stream task 側から閉じても main ループに届く
- ただし `Session::close` だけでは wire に `CONNECTION_CLOSE` が出ない。
  transport close を送るのは `MoqtClient::drain_events` の `SessionEvent::CloseSession(err) => { self.handle.close(err.code, err.reason).await?; return Ok(()); }` だけである。
  このイベントは `take_notable_event` が先に取り出してアプリへ返すため `drain_events` には残らない。
  main ループは `CloseSession` を受けると `break 'main` して `client.close(0, "")` を呼ぶが、`Session::fail` は `Closing` / `Closed` では早期 return し、`drain_events` のキューにもイベントが無いため transport close は発行されない
- `LocProperties::decode` の失敗には `KeyValueFormattingError` のほかに `UnexpectedEof` と `ProtocolViolation` (重複プロパティ ID など) もある
- ただし `UnexpectedEof` / `ProtocolViolation` は example には到達しない。
  subgroup 経路では `extract_*` の前に `data_plane.recv_subgroup_object` が呼ばれ、`ObjectPropertyTracker::observe_object` が同じバイト列を `ObjectProperties::decode` へ通す。
  FETCH 経路も `FetchStreamDecoder::validate_fetch_object` が同じ `observe_object` を呼ぶ (`Session::recv_fetch_entry` 自身は properties を受け取らず `FetchStreamDecoder` も保持しないが、`try_decode_entry` → `resolve_and_transition` の時点で検証される)。
  `src/object_properties.rs` の `decode_kv_pairs` は `varint::checked_len` による切り詰めと重複種別を `LocProperties::decode` と同じ規則で拒否するので、`KeyValueFormattingError` 以外の失敗は decoder / session 層が先に `ProtocolViolation` として該当 subscription を cancel する

## 設計方針

- `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` の戻り値を「プロパティ無し」と「書式違反」で区別できる形 (`Result<Option<T>, MessageError>` など) にする。`properties_bytes` が `None` の場合と `LocProperties::decode` が失敗した場合を同じ値にしない
- `MessageError::KeyValueFormattingError` を受けたら §8.3 の MUST どおり `SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`) でセッションを閉じる。プロパティが単に無い場合は従来どおり config / Timestamp 無しで再生を続ける
- `KeyValueFormattingError` 以外の decode 失敗 (`UnexpectedEof` / `ProtocolViolation`) も黙殺せず `SESSION_PROTOCOL_VIOLATION` (`0x3`) でセッションを閉じる。現行の subgroup / FETCH 経路では decoder / session 層が先に拒否するため到達しない防御だが、`Result` の契約としては現れうる
- セッションを閉じるのは main ループ (example の I/O 層) にする。stream task から直接 `Session::close` や transport close を呼ぶと、完了条件の観測が次の 2 つの経路で壊れるためである
  - main ループの `tokio::select!` の先頭分岐は `result = recv_acceptor.accept_recv_stream(), if accepting =>` であり、接続が閉じると `Err` 側の `Failed to accept data stream` で `break 'main` する。`Session closed: 0x6 ...` のログは出ない
  - `Session::close` が積む `SessionEvent::CloseSession` は `take_notable_event` と `drain_events` のどちらか一方しか取り出せない。`drain_events` が先に取り出すと `MoqtClient::drain_events` の `CloseSession` 分岐が transport close を送って即座に return するため、アプリはイベントを観測できない
- 実装は次の形にする
  1. `examples/moqt-subscriber/src/pipeline.rs` の `run` で `tokio::sync::mpsc::channel::<(u64, &'static str)>(1)` を作り、送信側を `spawn_stream_task` → `handle_incoming_stream` → `handle_stream_body` → `decode_video_stream` / `decode_audio_stream` へ渡す
  2. stream task は decode 失敗を検出したら `try_send((code, reason))` して処理を止める。`Session` や transport は閉じない。code は `KeyValueFormattingError` なら `SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`)、それ以外は `SESSION_PROTOCOL_VIOLATION` (`0x3`) にする。
     `KeyValueFormattingError` / `ProtocolViolation` が持つメッセージは `&'static str` のためそのまま渡せる。`UnexpectedEof` はメッセージを持たないため静的な文字列リテラルを使う
  3. main ループの `tokio::select!` に受信分岐を追加し、受信したら既存の `MoqtClient::close(code, reason)` を呼ぶ。`MoqtClient::close` は `Session::close` の後に `drain_events` を回すため、その `CloseSession` 分岐が `StreamHandle::close(err.code, err.reason)` を呼び、`0x6` の `CONNECTION_CLOSE` (WebTransport では CLOSE_SESSION capsule) が送出される
  4. `close` の失敗は `?` で伝播させず `tracing::warn!` に留め (`break 'main` 前の既存の `client.close(0, "")` と同じ扱い)、`Session closed: {:#x} {}` の形式でログを出して `break 'main` する。`peer_ended = true` も立て、確立済みでない session への `stop_sending` / `send_goaway` を走らせない。main ループ自身が閉じるため、accept 分岐や `drain_events` との競合でログが落ちることはない
- 呼び出し元は `decode_video_stream` (`extract_video_config`) と `decode_audio_stream` (`extract_audio_config` / `extract_timestamp_timescale`)。0124 の実装後は `handle_fetch_stream` も同じ扱いにする
- library (`src/loc.rs` / `src/session/`) は変更しない。検証は実装済みで、欠けているのは example 側の扱い

## 完了条件

- Video Frame Marking の宣言長 5、Audio Level 256 のいずれかを含む Object Properties を送る peer に対して、subscriber example が `SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`) の `CONNECTION_CLOSE` を送出し、main ループが `Session closed: 0x6 ...` を出して終了すること。次の 4 つを満たすこと
  - `examples/moqt-subscriber/src/pipeline.rs` の `#[cfg(test)] mod tests` に、`extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` が `KeyValueFormattingError` をエラーとして返し、`properties_bytes` が `None` の場合 (プロパティ無し) と区別されることを固定する unit test が追加されていること
  - 既存の `extract_audio_config` の unit test 3 件 (`extract_audio_config_returns_bytes_when_present` / `extract_audio_config_returns_none_when_absent` / `extract_audio_config_returns_none_on_bad_properties`) を新しい戻り値に合わせて更新すること。
    `decode_audio_stream` の呼び出し箇所 (`&& let Some(config_bytes) = extract_audio_config(...)` と `let (timestamp, timescale) = extract_timestamp_timescale(...)`) も同様に更新する。
    特に `extract_audio_config_returns_none_on_bad_properties` は `Some(&[0xFF, 0xFF, 0xFF])` に `None` を期待しているが、`[0xFF, 0xFF, 0xFF]` は 8 バイト形 varint の途中終端で `UnexpectedEof` になり、プロパティ無しの `None` とは区別される
  - セッションが閉じることを確認する手順を、issue の完了時に「実機確認の結果」として記録すること。手順は次のとおり
    - `LocProperties::encode` が違反値を拒否するため `build_video_loc_properties` の改変では再現できない。
      `examples/moqt-publisher/src/stream_writer.rs` の `write_object` の `Some(properties.encode()?)` を一時的に `Some(vec![0x07, 0x09, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05])` (宣言長 5 の Video Frame Marking) へ差し替える。
      `properties.is_empty()` の分岐は残し、カタログ送信 (`has_properties = false`) に影響させない
    - publisher を起動し subscriber を接続する。subscriber 側に `Session closed: 0x6 ...` のログが出て main ループが終了すること、publisher 側が接続クローズで終了することを確認する (publisher 側のメッセージに `0x6` が現れる場合はその値も記録する)
    - 確認後は一時改変を戻す
  - `CHANGES.md` の `## develop` に `[FIX]` エントリを追加すること
    (`- [FIX] moqt-subscriber が LOC の書式違反を検出したら KEY_VALUE_FORMATTING_ERROR (0x6) でセッションを閉じるようにする` とその補足、`@voluntas`)
- 正常な Video Frame Marking (1-4 バイト) と Audio Level (8 bit 範囲) を受信した場合、および `properties_bytes` が `None` の場合の既存挙動 (config / Timestamp 無しでも再生を継続する) が変わらないこと
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 解決方法

`LocProperties::decode` の失敗を「プロパティ無し」に潰していた 3 つの抽出関数を `Result` 化し、書式違反をセッション終了として扱うようにした。close は I/O 層である main ループが行う。

1. `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` の戻り値を `ExtractResult<T>` (`std::result::Result<T, MessageError>`) にした。`Ok(None)` / `Ok((None, None))` は「Properties が無い、または対象のプロパティを持たない」、`Err` は decode 失敗である
2. `session_error_code` が decode 失敗を終了コードへ写す。`MessageError::KeyValueFormattingError` は §8.3 (Key-Value-Pair Structure) の
   MUST に対応する `SESSION_KEY_VALUE_FORMATTING_ERROR` (`0x6`)、それ以外 (`UnexpectedEof` / `ProtocolViolation` など) は `SESSION_PROTOCOL_VIOLATION` (`0x3`) にする。
   理由文は `MessageError::reason()` (`&'static str`) をそのまま使う
3. 呼び出し側 (`handle_fetch_stream` / `decode_video_stream` / `decode_audio_stream` の 4 箇所) は `Err` を受けたら `request_session_termination` で main ループへ `SessionError` (library の型) を送り、その stream の処理を打ち切る
4. main ループの `tokio::select!` に終了依頼の受信分岐を追加し、`MoqtClient::close(code, reason)` を呼んで
   `Session closed: {:#x} {}` を記録して `break 'main` する。transport close (QUIC では `CONNECTION_CLOSE`、
   WebTransport では CLOSE_SESSION capsule) を送出できるのは `MoqtClient` を所有する main ループだけであり、
   stream task から直接 `Session::close` を呼ぶと main ループが `SessionEvent::CloseSession` を観測できないまま
   accept 分岐が接続クローズを拾って `Session closed: ...` のログが落ちる
5. 自側から終了コード付きで閉じた場合は `session_terminated` フラグを立て、close 済みの session へ `STOP_SENDING` / `GOAWAY` /
   正常 close を送らない。これらは状態機械に拒否され "Failed to send GOAWAY" などの誤解を招く警告ログになるためである。
   送信有無の判定は `should_stop_sending` / `should_close_gracefully` に切り出し、単体テストで固定した。
   `select!` の別分岐 (GOAWAY / PUBLISH_DONE / peer の close / shutdown) が先に成立した場合も、
   受信ループを抜けた直後に届いている終了依頼を回収し、§8.3 の MUST を終了コード 0 で上書きしない
6. 終了依頼のチャネルは容量 1 で、既に依頼が積まれている間の 2 件目は送らない (最初に検出した失敗のコードで閉じる)。`select!` の受信は `Some(termination) = ...` で受ける (送信側の原本を main ループが保持するため受信ループ中はチャネルが閉じない)
7. 配送しない Object (`FilteredOut` / `Discarded`) と payload を持たない Object は Properties を解釈しないため、書式違反の検出対象外である旨をコメントに残した。datagram 経路とカタログ FETCH は LOC のメディア プロパティを扱わないため対象外である

テスト:

- `examples/moqt-subscriber/src/pipeline.rs` の `#[cfg(test)] mod tests` を次のように更新・追加した
  - `extract_video_config_returns_config` / `extract_video_config_returns_none_without_config` / `extract_video_config_returns_none_when_absent` / `extract_video_config_reports_eof_on_truncated_properties`: 正常値・対象プロパティ無し・Properties 無し・切り詰めを固定する
  - `extract_video_config_reports_key_value_formatting_error`: Video Frame Marking の宣言長 5 (draft-ietf-moq-loc-04 §2.3.2.2 の 1-4 バイト違反) が `KeyValueFormattingError` になることを固定する (`LocProperties::encode` は違反値を拒否するため生バイト列を組み立てる)
  - `extract_audio_config_returns_bytes_when_present` / `extract_audio_config_returns_none_when_absent` /
    `extract_audio_config_reports_eof_on_truncated_properties`: Audio Config の正常値・無し・切り詰めを固定する。
    `extract_audio_config_returns_none_on_bad_properties` は「壊れた properties は `None`」という旧契約をやめ、
    切り詰め (`[0xFF, 0xFF, 0xFF]` は 8 バイト形 varint の途中終端で `UnexpectedEof`) を固定する
  - `extract_audio_fields_report_key_value_formatting_error`: Audio Level 256 (8 bit 範囲違反) が両方の抽出で `KeyValueFormattingError` になることを固定する
  - `extract_timestamp_timescale_returns_none_when_absent`: Timestamp / Timescale 無しの `Ok((None, None))` を固定する
  - `session_error_code_maps_decode_failures`: `KeyValueFormattingError` → `0x6`、`ProtocolViolation` / `UnexpectedEof` → `0x3` の写像を固定する
  - `cleanup_decisions_follow_termination_cause`: 終了要因の組み合わせごとに STOP_SENDING / GOAWAY / 正常 close の送信有無が決まることを固定する
  - `request_session_termination_sends_error_and_keeps_first_request`: 終了依頼が `SessionError` としてチャネルに届くこと、容量 1 のため先に積まれた依頼が後続で置き換わらないことを固定する (`tokio::time::timeout` で包み、送信しない実装ではハングせず失敗する)
- 変異実験で検出力を確認した (写像を常に `0x3` にする / `try_send` を無効化する / 抽出関数がエラーを潰す / Video Frame Marking の値域検証を無効化する / Audio Level の値域検証を無効化する の各変異で対応するテストが失敗する)

実機確認の結果:

- 接続先は sora-moq の local clone の dev relay (`_build/dev/rel/sora_moq/bin/sora_moq foreground`、`moqt://127.0.0.1:4433`) を使った
- 正常系: 無改変の publisher (`--fake-capture-device`) と subscriber を接続し、subscriber が video 645 フレーム / audio 1207 チャンクを再生できることを確認した
- 異常系: publisher の `examples/moqt-publisher/src/stream_writer.rs` の `write_object` を一時的に `Some(vec![0x07, 0x09, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05])` (宣言長 5 の Video Frame Marking) へ差し替えてビルドし、ソースは元に戻したうえでその binary を実行した
  - subscriber 側は次のログを出し、`Pipeline stopped: 1 streams received` を経て exit code 0 で終了した。
    `Failed to send GOAWAY` の警告は出ておらず、自側終了時の後始末スキップが機能している
    - `WARN moqt_subscriber::pipeline: Closing session on LOC property error: 0x6 Video Frame Marking length must be 1-4 bytes (draft-ietf-moq-loc-04 §2.3.2.2)`
    - `INFO moqt_example_transport::moqt_client: Session closed with code 0x6 Video Frame Marking length must be 1-4 bytes (draft-ietf-moq-loc-04 §2.3.2.2)`
    - `WARN moqt_subscriber::pipeline: Session closed: 0x6 Video Frame Marking length must be 1-4 bytes (draft-ietf-moq-loc-04 §2.3.2.2)`
    - `WARN moqt_example_transport::moqt_client: Failed to accept peer bidi stream: QUIC error: The connection was closed on the application level with error application::Error(6) by the local endpoint`
  - relay 側 (peer の観測): `INFO MOQT transport closed: {connection_closed,6,<<>>}` を記録した。QUIC の CONNECTION_CLOSE が application error code 6 (`KEY_VALUE_FORMATTING_ERROR`) で送出されたことを peer 側からも確認できる
- publisher は relay との接続を維持したままだった。relay が間に入る構成では下流 (subscriber) の close は上流 (publisher) へ伝播しないためである (issue の完了条件にある「publisher 側が接続クローズで終了する」は publisher と subscriber が直接接続する構成を想定した記述であり、relay 経由の本確認では観測されない)
- 確認後は publisher の一時改変を revert し、起動した relay も停止した
