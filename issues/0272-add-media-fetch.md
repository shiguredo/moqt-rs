# メディアの Object を FETCH で取得できるようにする

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/add-media-fetch
- Polished: 2026-10-15

## 目的

draft-ietf-moq-transport-22 §3.2 (Fetch) の FETCH は、Start Location から End Location までの
既存 Object を subscriber が取得するための要求である。§9.5.1 は Largest Object 以下の任意の
Object を FETCH で取得できると定め、§3.5.1 は購読中に現在の Group の先頭まで FETCH で
遡るパターンを示している。

取得する range は fill fetch (§3.4) でも subscriber が `FILL_PARAMETERS` の Location filter で
指定する。両者の違いは、subscriber が FETCH 要求 stream を開くか、SUBSCRIBE / REQUEST_UPDATE に
`FILL_PARAMETERS` を載せて publisher に fill fetch stream を開かせるか、および成否の
受け取り方 (FETCH_OK / REQUEST_ERROR か、FIN / RESET か) である。§3.4.1 は fill fetch stream に
REQUEST_ERROR が無く、publisher が reset で失敗を伝えると定める。

現状は publisher にメディアの保持が無く、catalog 以外の FETCH に応答できない。
subscriber も FETCH を発行できないため、配信の途中参加で過去の Group を取得できない。
FETCH による取得を可能にし、途中参加時にキーフレームまで遡って再生できるようにする。

メディアの fill fetch は本 issue では扱わず、FETCH 要求 stream による取得に限定する。

## 現状

- ライブラリは FETCH の要求側と応答側を実装済み: `Session::send_fetch`、
  `SessionEvent::FetchOkReceived` (`end_location` / `end_of_track`)、`FetchStreamDecoder` /
  `FetchStreamEncoder`、§3.2.2 の End of Non-Existent / Unknown / Timed-Out Range の encode / decode
- §3.2 の MUST 2 条件 (publish 実績が無い track / Start Location が Largest Object より大きい) と
  空 range の `INVALID_RANGE` は `Session` が判定して REQUEST_ERROR を送る
  (`src/session/fetch.rs` の `handle_peer_fetch`)。`Session::recv_request` が受理しなかった
  request も bidi 受信ループは結果を区別せず `ClientEvent::Request` としてアプリへ渡すため、
  アプリが重ねて応答すると `Session::send_fetch_ok` / `send_request_error` が未登録の
  Request ID を拒否して致命終了する。catalog の FETCH にも同じ経路がある
- `examples/tokio-moq` の `MoqtClient` に FETCH を発行する API が無い。0224 で catalog 取得を
  `FILL_PARAMETERS` 付き SUBSCRIBE に寄せたあと、到達不能な公開 API として 0235 で削除している
- `MoqtClient` は `SessionEvent::FetchOkReceived` をアプリへ配送しない (`is_notable_event` の
  対象外で、`drain_events` の該当 arm もコメントで「本 example は FETCH を発行しないため
  到達しない」としている)。FETCH を発行しても応答を待てない
- FETCH 応答を送る API (`MoqtClient::send_fetch_response` / `send_fill_fetch_response`) は
  呼ぶたびに uni stream を開いて Object を 1 件書いて FIN する。保持した複数の Object と
  End of Range を 1 本の応答 stream に載せる API が無く、`FetchStreamEncoder` は
  `examples/` から使われていない
- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は catalog の FETCH だけに応答し、
  メディアの FETCH は `REQUEST_DOES_NOT_EXIST` で拒否する。`run` の `OpenFillFetchStream` 処理も
  `reject_fill_fetch` で catalog 以外を拒否する
- moq-pub が保持するのは最後の catalog JSON と Location (`catalog_json` / `catalog_location`) だけで、
  メディアの payload も LOC Properties も保持しない
- `examples/moq-sub/src/pipeline.rs` は FETCH を発行しない。届いた fill fetch stream は
  `handle_stream_body` の `StreamType::Fetch` で読み捨てる
- MOQT の Track Property `PROP_MAX_CACHE_DURATION` (`0x04`、§10.3) は
  `src/track_properties.rs` にあるが、publisher は告知していない
- `examples/tokio-moq/src/peer_harness.rs` の `send_request_start` は任意の制御メッセージを
  request stream の先頭に送れるため peer から FETCH を発行できるが、FETCH_OK と FETCH 応答
  stream を返す経路が無く、client が FETCH を発行して応答を読む流れを固定できない

## 設計方針

### 対象とする機構

- subscriber が発行する FETCH メッセージ (§3.2 / §9.11) と、それに対する publisher の
  FETCH 応答 (§9.12 / §11.4) を対象とする
- メディアの fill fetch (§3.4) は対象外とする。moq-pub は従来どおり catalog 以外の
  `OpenFillFetchStream` を `reject_fill_fetch` で拒否し、moq-sub もメディアに
  `FILL_PARAMETERS` を付けない

### publisher の保持

- moq-pub にメディアの保持領域を追加する。`--cache-duration` (ミリ秒、既定 5000) の時間窓で
  映像と音声の Object を Group 単位でメモリに保持し、窓を過ぎた Object を evict する。
  保持の期限は Object を保持した時刻 (publisher の壁時計) で判定する。Group ID は配信開始時に
  Unix epoch ミリ秒から払い出すが、以降は `group_id::next_group_id` で +1 するため
  (`examples/moq-pub/src/group_id.rs`)、Group ID から経過時間は復元できない
- 期限切れの評価は、保持への追加時と FETCH への応答時の双方で行う。追加時だけだと窓を過ぎた
  Object を配り得て、告知値と実保持が一致しなくなる
- 保持量の上限は時間窓だけで決める。Group 数や byte 数の上限を別に設けると、トラックごとに
  実際に保持できる時間が変わり (音声は 1 Object = 1 Group で 20 ms 間隔)、§10.3 の
  `MAX_CACHE_DURATION` が示す「保持から配れる時間」と一致しなくなる
- 保持するのは payload と LOC Properties の両方とする (FETCH 応答には両方が要る)
- 告知する `MAX_CACHE_DURATION` (§10.3) は `--cache-duration` と同じ値にする。
  告知先は `video_track_properties` と同じ `TrackProperties` の経路 (映像 / 音声の PUBLISH と
  SUBSCRIBE_OK) とする

### FETCH への応答

- 応答の判断は I/O から独立した関数として example に切り出し、単体テストで固定する。
  入力は「PUBLISH した track か」「publish 済みの Largest Object」「要求 range」
  「要求の `GROUP_ORDER`」「その track の保持内容」で、結果は次の 4 種とする。
  判定の順序は対象外の track → `GROUP_ORDER` → 保持内容とする。Session が受理した FETCH
  だけがアプリへ渡るため、受理の判定はアプリ側で再計算しない (再計算すると、拒否済みの
  request に応答して致命終了するか、受理済みの request に無応答になる)
  1. 対象外の track (PUBLISH していない): 従来どおり `REQUEST_DOES_NOT_EXIST` を返す。
     Session は publish 実績の無い track の FETCH を受理するため、応答が必要になる
  2. `FETCH_OK` + 保持 Object + End of Unknown Range: 要求 range に保持している Object がある。
     End Location は要求の終端を Largest Object でクランプした値にする
  3. `FETCH_OK` + 要求 range 全体を End of Unknown Range: 要求 range の Object を 1 つも
     保持していない (期限切れで evict 済み)。「存在しない」ではなく「不明」である
     (§2.1.2 / §10.3)
  4. `REQUEST_ERROR(NOT_SUPPORTED)`: 要求の `GROUP_ORDER` が Descending (`0x02`)。
     §3.2.1 は要求された group order で送る MUST を課すため、昇順で返さない
- End of Unknown Range (`0x10C`) は「直前に書いた entry の Location から自分の Location までの
  区間」を表す (§11.4.1.2)。位置によって意味が変わるため、次を守る
  - 要求 Start が最初に serialize する Object より前の場合にだけ、その Object の前に置く。
    Location は要求 range 内で evict された最後の Location (要求 Start 以上、直後の Object
    未満) にする。要求 range 内に evict された Object が無ければ何も書かない
    (§3.2.2 の既定で存在しないものとして扱われる)
  - 時間窓で evict されるのは古い側だけであるため、保持範囲の内部には欠落を作らない
  - 要求 range 全体が不明な場合は、その 1 件だけを書き、Location は `FETCH_OK` の
    End Location にする
  - 直前に書いた entry の Location 以下の Location は書かない
- 応答は 1 本の uni stream に FETCH_HEADER を書き、Object と End of Range を順に書いて FIN する。
  `examples/tokio-moq` に `FetchStreamEncoder` を使う応答 writer を追加する。既存の
  `send_fetch_response` (Object 1 件で FIN) は catalog の応答が使っているため残し、
  「1 件だけ返す経路」と「保持範囲を返す経路」を用途で書き分ける
- 要求の `GROUP_ORDER` が Descending (`0x02`) の場合は昇順の保持範囲を返さず、
  `REQUEST_ERROR(NOT_SUPPORTED)` で拒否する (応答判断の 4 種目)。Descending で返す実装は
  本 issue では扱わない (対象外)

### transport 層

- `MoqtClient` に FETCH を発行する API を追加する (`Session::send_fetch` を呼び、
  `request_id` を返す)。`with_auth_parameters` の付与対象に FETCH を加える
  (draft-ietf-moq-msf-01 §11.4.3)
- `MoqtClient` は、`Session::recv_request` が受理しなかった FETCH をアプリへ渡さない
  (`Session::fetch(request_id)` が `None` の FETCH を `BidiMessage::PeerRequestRejected` と
  同じ扱いにする)。アプリが応答するのは Session が受理した FETCH だけになる
- `SessionEvent::FetchOkReceived` をアプリへ配送する (`is_notable_event` と `drain_events` を
  変更し、0235 の「FETCH を発行しない」前提のコメントも更新する)
- FETCH 応答の writer は `FetchOkReceived` を待つ経路とは独立に提供する

### subscriber の取得

- 購読で得た Largest Object (§3.4 の `LARGEST_OBJECT`。`SubscribeResult::largest_object`) を
  基準に、直近の Group を含む range を FETCH で要求し、
  取得した Object を通常の再生経路 (`handle_stream_body` など) に流す。
  映像はキーフレームから復号を始める
- 要求 range の決定と、FETCH 応答 stream の Request ID からどの track の応答かを解決して
  Object と End of Range を振り分ける判断は、I/O から独立した関数として切り出し、
  単体テストで固定する。End of Range は取得できなかった範囲として扱い、ログに残す
- 任意位置シークの操作 UI と、VOD (`isLive=false` / `trackDuration`) の再生は本 issue では
  扱わない

## 対象外

- メディアの fill fetch (§3.4) と、`send_fill_fetch_response` への LOC Properties の追加
- catalog の FETCH 応答 (`catalog_fetch_response`) の作り直し。受理しなかった FETCH を
  アプリへ渡さない変更が catalog 経路の二重応答も防ぐ
- 要求の `GROUP_ORDER` が Descending の場合
- 任意位置シークの操作 UI と VOD の再生
- 実 relay 越しのエンドツーエンド確認。relay が §3.2.3 の upstream FETCH を発行して publisher に
  届ける必要があり、本 issue では行わない (`cargo test --workspace` で検証できる範囲を
  完了条件とする)

## 完了条件

- `examples/tokio-moq/src/peer_harness.rs` を拡張し、次がテストで固定されていること
  - client 発: `MoqtClient` の FETCH 発行 → `FetchOkReceived` (`end_location` / `end_of_track`)
    の受信 → 応答 stream の Object と End of Range 3 種の受信
  - client 着: 応答 writer が 1 本の uni stream に FETCH_HEADER + 複数 Object +
    End of Unknown Range を書いて FIN し、peer 側の `FetchStreamDecoder` が同じ内容を
    decode できること
- Session が受理しなかった FETCH がアプリへ渡らないことが peer_harness のテストで
  固定されていること (拒否された FETCH に応答しようとしても致命終了しないこと)
- moq-pub の応答判断が純関数として切り出され、「対象外の track (REQUEST_DOES_NOT_EXIST)」
  「保持 Object + End of Unknown Range」「要求 range 全体が End of Unknown Range」
  「Descending の `REQUEST_ERROR(NOT_SUPPORTED)`」の 4 分岐が単体テストで固定され、
  End of Unknown Range の Location が要求 range の内側に収まることも固定されていること
- moq-pub が `MAX_CACHE_DURATION` を `--cache-duration` と同じ値で告知することが、
  告知値の決定を純関数として切り出した単体テストで固定されていること
- moq-sub の FETCH 要求 range の決定と、応答 Object / End of Range の振り分けが純関数として
  切り出され、単体テストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
