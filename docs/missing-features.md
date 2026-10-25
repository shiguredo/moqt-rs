# moq-pub / moq-sub と仕様の差分 (足りていない機能)

調査日: 2026-10-14 (develop の f91baf3 時点)。

最終更新: 2026-10-24 (§6.6 の GOAWAY によるセッション移行のみ現状に更新)。

対象仕様は `refs/moq/` の MOQT `draft-ietf-moq-transport-22`、LOC `draft-ietf-moq-loc-04`、
MSF `draft-ietf-moq-msf-01`、C4M `draft-ietf-moq-c4m-01` である。
対象コードは `src/` (ライブラリ)、`examples/moq-pub`、`examples/moq-sub`、
`examples/tokio-moq` (transport 層)。

`CODEBASE.md` の「クライアントとサーバーのみで MoQ リレーには対応しないこと」により、
MoQ リレー専用の機能 (`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` / `SUBSCRIBE_TRACKS` /
`NAMESPACE` / `NAMESPACE_DONE` / `PUBLISH_SKIPPED`、§7 全体、§3.2.3 Relay Fetch Handling、
§7.4.1 / §7.6.1 Relay Switchover、namespace 発見・forwarding) は本稿の対象外とする。
`docs/moqt.md` / `docs/msf.md` / `docs/loc.md` / `docs/c4m.md` の「未対応」に
書かれている内容も既知として扱い、重複して数え上げない。

## 1. 結論

ライブラリ (`src/`) のプロトコル被覆は高い。draft-22 の制御メッセージ、Setup Options (7 種すべて)、
メッセージパラメータ、Track / Object Property、エラーコードはほぼ実装済みである。
足りていないのは次の 4 層に分かれる。

1. **仕様側で明示的に未実装のまま残っている機能**: Secure Objects (LOC §3 / MSF §4.3)、
   MSF の catalog 圧縮 (§5.5)、Log / Metrics track の payload (§9 / §10)、
   LOC の Video Payload Format (§2.1)。いずれも `issues/pending/` に起票済みである。
2. **ライブラリには API があるのに、moq-pub / moq-sub から使われていない機能**:
   FETCH (要求側 API は transport 層で削除済み)、TRACK_STATUS、Media / Event Timeline、
   MSF の変数置換、パディング、`publishTracks`、Audio Level、`END_OF_GROUP`、
   Range Filters を有効にする `MAX_FILTER_RANGES` など。
3. **アプリ機能として欠けているもの**: request stream GOAWAY の非空 URI による個別 request
   移行 (2 session の併走が必要)、REQUEST_ERROR の Redirect 追随、トラック選択と切替、
   シーク / 巻き戻し、ライブ中の track 追加への追随、配信終了のシグナリング、統計表示。
4. **MUST 未達・仕様違反になりうる箇所**: Object 側 `IMMUTABLE_PROPERTIES` の探索 (§10.7)、
   `isLive=false` の `targetLatency` (MSF §5.2.8)、catalog 更新をまたいだ
   `isLive` の False → True (MSF §5.2.7)、`connectionUri` の検証と新規接続 (MSF §5.2.36)、
   range パラメータの union (MSF §11.1.1)、`initDataList` の配置 (§5.1.7)、
   STOP_SENDING 受信時の open stream の reset (§3.1.2 / §3.2.4)。

以下、根拠となる仕様の節とコード位置を添えて列挙する。

## 2. 仕様側で未実装 (issue 起票済み)

| 機能 | 仕様 | 現状 | issue |
| --- | --- | --- | --- |
| Secure Objects による E2E 暗号化と復号 | LOC §3、MSF §4.3 | 未実装。MSF の signaling フィールドは扱うが、`ENCRYPTED_LIST` (`0x0A`) / `PADDING` (`0x32`) Object Property と暗号処理が無い | `issues/pending/0004-add-secure-objects-encryption.md` |
| MSF_COMPRESSION property による catalog 圧縮 | MSF §5.5 / §12.1 | 未実装。Track Property ID が TBD のため wire 実装しない | `issues/pending/0002-change-msf-compression-signaling.md` |
| Log / Metrics track の payload | MSF §9 / §10 | 未実装。`packaging` の値は扱うが payload は未対応 | `issues/pending/0003-add-msf-log-metrics-payload.md` |
| LOC の Video Payload Format | LOC §2.1 | 部分実装。`lengthSizeMinusOne` による length prefix のみで、§2.1.3 (length 1 = start code) と §2.1.4 (start code prefix) は未対応 | `issues/pending/0005-add-loc-video-payload-format.md` |
| LOC の Track スコープ Property | LOC §6.1 Table 1 | 未実装。`TIMESCALE` / `VIDEO_CONFIG` / `AUDIO_CONFIG` は Track スコープでも定義されるが Object スコープでしか送らない | `issues/0125-add-track-scope-loc-properties.md` |
| WebTransport over HTTP/3 の `RESET_STREAM_AT` | draft-ietf-webtrans-http3-16 §4.4 (MUST) | s2n-quic が送出できず `RESET_STREAM` にフォールバックする | `issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md` |
| DPoP proof の送信 | C4M §3.2.2 | ライブラリに発行 API はあるが、example の URL 経路では送らない | 未起票 |
| C4M の複合クレーム / RSA / CWT 形式 proof | C4M §2 | 未実装 (docs/c4m.md の「未対応」に記載) | docs/c4m.md |

## 3. ライブラリの API 露出と example での未使用

`src/` の公開 API のうち `examples/` から呼ばれていないもの、および draft の MAY に
対応していないものを機械的に抽出した。「実装済みだがクライアント機能として
露出していない」ものである。

| 機能 | API / 現状 | 仕様 | 影響 |
| --- | --- | --- | --- |
| FETCH の要求 | `Session::send_fetch` はあるが、`MoqtClient` に発行 API が無い (削除済み) | §3.2 / §9.11 | moq-sub は過去の Object を取得できない。シーク、巻き戻し、切断後の追いつきができない |
| TRACK_STATUS | `send_track_status` / `forget_track_status` | §9.13 | 購読前にトラックの存在・最新 Object を確認できない。moq-pub 側は受信しても `NOT_SUPPORTED` で拒否する |
| Media / Event Timeline | `msf::encode_media_timeline` / `encode_event_timeline` | MSF §7 / §8 | moq-pub はタイムラインを配信せず、moq-sub も受信しない |
| Timeline のテンプレート展開 | `MsfTemplate::resolve_entry` | MSF §7.4 | テンプレート形式の Timeline を使えない |
| MSF の変数置換 | `msf::resolve_catalog_variables` | MSF §5.4 / §5.2.43 | moq-pub が `authInfo` に `"%c4m%"` を書くのに、moq-sub が解決しないため認可トークンの受け渡しが example 間で成立しない |
| MSF fragment の範囲指定 | `msf::uri` の `wallclock_ranges` / `mediatime_ranges` / `location_ranges` | MSF §11.1.1 | 範囲指定を解釈せず全期間を購読する。複数指定の union MUST も未実装 |
| `MAX_FILTER_RANGES` の宣言 | `SETUP_OPTION_MAX_FILTER_RANGES` | §9.1.6 | 既定 0 のため、購読者は Range Filters (§3.3.2) を送ってはならない。ライブラリのフィルタ評価実装が活きない |
| `MAX_REQUEST_UPDATES` / `MAX_AUTH_TOKEN_CACHE_SIZE` | `SETUP_OPTION_*` | §9.1.7 / §9.1.3 | 既定値で実害は無いが、上限を宣言しない |
| パディング | `send_padding_stream` / `send_padding_datagram` / `recv_padding_datagram` | §11.5 | 帯域の押し上げや MTU 探索に使えない (優先度は低い) |
| RESET_STREAM_AT | `reset_outgoing_data_stream_at` | webtrans-http3 §4.4 | WebTransport 経路で MUST の reset ができない (0094 と同じ原因) |
| `PRIOR_GROUP_ID_GAP` | `ObjectProperties::prior_group_id_gap` | §10.8 / MSF §6.1 | moq-pub は前回の Group ID を永続化しないため gap を宣言できない (SHOULD の前提を満たさない) |
| `END_OF_GROUP` | Subgroup Header / Object Datagram のフラグ | §11.2.1 / §11.3.1 | moq-pub は常に false を送る。音声は 1 Group = 1 Object のため、終端を早期に通知できない |
| `publishTracks` | `MsfCatalog::publish_tracks` | MSF §5.1.5 / §5.6.16 | 逆方向 track (Log / Metrics) を告知できず、QoE メトリクスの返送経路が無い |
| `initDataList` / `initRef` / `buffers` | `MsfInitData` / `MsfBuffers` | MSF §5.1.7 / §5.2.9 / §5.2.13 | moq-sub は in-band の Video Config / Audio Config 前提で、initData 経由の構成情報を扱わない |
| `buffers` / `maxGopDuration` / `maxGroupDuration` | `MsfBuffers` / `MsfTrack` | MSF §5.2.9 / §5.2.24 / §5.2.25 | moq-pub は宣言するが moq-sub は読まない。バッファ下限 / 上限の指示が再生に反映されない |
| `altGroup` | `MsfTrack::alt_group` | MSF §5.2.12 / §4.2 | 品質切替 (ABR) の告知も選択もできない |
| Audio Level | `PROP_AUDIO_LEVEL` | LOC §2.3.3.2 | moq-pub が送らず moq-sub も読まない |
| catalog の delta update 生成 | `MsfCatalogDocument::Delta` | MSF §5.1.6 / §5.3 | moq-pub は常に Full のみ。MAY なので違反ではない |
| `PUBLISH_STATE_NOTIFY` の送信 | `Session::send_publish_state_notify` に `MoqtClient` のラッパーが無い | §9.10 | publisher から状態変化を通知できない |
| PUBLISH_OK 前の Object 送信 | `send_subgroup_header` / `send_object_datagram` が `Established` を要求する | §3.1 (MAY) | publisher は PUBLISH_OK を待つ分だけ購読確立が遅れる (違反ではない) |
| FETCH_OK 前の Object 送信 | `send_fetch_header` が `FetchState::Established` を要求する | §3.2 (MAY) | End Location が確定するまで FETCH 応答 Object を送れない。fill fetch は `Pending` を許容するため非対称 |
| moqt URI のパース | `msf::uri` は fragment 必須で `msf:` 以外を拒否する。authority / path / query の分解は example 側の `parse_url` にのみある | §6.1 / §6.1.1 / §6.1.2 | ライブラリ利用者は URI 解釈を毎回自前実装する (意図的に example へ置いている可能性あり) |

## 4. MUST 未達・仕様違反になりうる箇所

### 4.1 Object 側 `IMMUTABLE_PROPERTIES` を探索しない (§10.7、ライブラリ)

draft-22 §10.7 は「When looking for the value of a property, processors MUST search both the
mutable properties and the contents of Immutable Properties.」と定める。
MOQT の `ObjectProperties` 側は探索する実装がある (`src/object_properties.rs:266-300`) が、
LOC 側の `LocProperties` はトップレベルの KVP しか走査しない (`src/loc.rs:393-415`)。
moq-sub は 4 箇所で `LocProperties::decode` を呼んで Video Config / Timestamp を取り出すため、
`IMMUTABLE_PROPERTIES` の中に Video Config を入れる publisher の Object は
構成情報を取得できず、デコーダを初期化できないまま破棄される。
`issues/0125` は Track スコープ側を扱うが、Object スコープ側は射程外の可能性がある。

### 4.2 `isLive=false` の `targetLatency` を使う (MSF §5.2.8、moq-sub)

MSF §5.2.8 は「If isLive is FALSE, this field MUST be ignored.」と定める。
moq-sub は `targetLatency` を無条件に再生遅延へ反映する
(`examples/moq-sub/src/pipeline.rs:727-743`、`2150-2175`)。
`is_live` は example から参照されていない (grep で 0 件)。VOD のトラックで不要な遅延を入れる。

### 4.3 catalog 更新をまたいだ `isLive` の False → True を拒否しない (MSF §5.2.7、moq-sub)

MSF §5.2.7 は「A True value MUST never follow a False value.」と定める。
ライブラリは同一 catalog 内の delta 再追加では検証する (`src/msf.rs:1537` 付近の
`validate_readded_track`) が、moq-sub は独立カタログ (Full) を受けるたびに
直前のカタログを捨てて丸ごと差し替える (`examples/moq-sub/src/catalog.rs:118-130`)。
更新をまたいだ履歴が失われるため、`isLive=false` の後に `isLive=true` の
カタログを受けても受理する。`isComplete` の「一度 TRUE にしたら削除できない」
(§5.1.3) も同じ経路で検証されない。

### 4.4 `connectionUri` の検証と新規接続 (MSF §5.2.36)

MSF §5.2.36 は `connectionUri` が「valid MOQT endpoint URI でなければならない (MUST)」とし、
指定時は「購読者が新しい MOQT 接続を確立しなければならない (MUST)」と定める。
ライブラリは URI 形式の検証を行わないと明記し (`src/msf.rs:2810-2813`)、
example に `connectionUri` を参照する箇所も無い (grep で 0 件)。
publishTracks (Log / Metrics) を使う配信で壊れた URI を検出できず、
購読側も別接続を張らない。

### 4.5 range パラメータの union を実装していない (MSF §11.1.1)

MSF §11.1.1 は「If multiple ranges are specified within the same URL for the same parameter,
the client MUST process the union of those ranges.」と定める。
アクセサは `src/msf/uri.rs:139-172` にあるが、同じパラメータの複数指定を
union へ畳む処理も、範囲を購読 / FETCH に反映する経路も無い (example からの呼び出しは 0 件)。

### 4.6 `initDataList` の配置 MUST を decode で検証しない (MSF §5.1.7)

MSF §5.1.7 は「The Initialization Data List, if present, MUST be located after the
Tracks field.」と定める。ライブラリは `nojson` がキー出現順を提供しないため
順序検証を行わないと明記している (`src/msf.rs:2589-2591`)。encode は順序を守る。
仕様違反のカタログを受理するが実害は小さい。

### 4.7 STOP_SENDING 受信時に open な stream を reset しない (§3.1.2 / §3.2.4、ライブラリ)

§3.1.2 は「The Publisher can remove subscription state as soon as it has received
STOP_SENDING. It MUST reset any open streams associated with the SUBSCRIBE.」と定め、
§3.2.4 も FETCH について bidi request stream と uni data stream の reset を MUST とする。
ライブラリが自動で reset するのは fill fetch stream だけで
(`src/session/data.rs:1914`)、送信中の subgroup stream と FETCH の uni data stream は
reset しないと明記している (`src/session/types.rs:750-756`)。
アプリが `send_data_stream_closed` / `send_fetch_data_stream_closed` を呼ぶ前提であり、
取りこぼすと Stream Count の確定 (PUBLISH_DONE) と cleanup が進まない。
`issues/0246-bug-cancel-outgoing-stream-cleanup.md` は順序契約の話で、
自動 reset の欠落そのものは起票されていない。

### 4.8 peer の SETUP を API から読めない (§6.3.2 / §9.1.5、ライブラリ)

`SetupState { local, peer }` は `pub(super)` で (`src/session/core.rs:67-71`)、
公開されているのは `peer_auth_token_cache` など一部だけである。
`SetupOptions` にも `MOQT_IMPLEMENTATION` の getter が無い (`src/parameter.rs:591-635`)。
アプリは peer が宣言した拡張 (Setup Option) を判定できず、§6.3.2 の
拡張ネゴシエーションを行えない。example のラッパーも SETUP を消費して捨てる
(`examples/tokio-moq/src/moqt_client.rs:1350`) ため上位アプリからも見えない。

## 5. publisher (moq-pub) に足りない機能

### 5.1 配信終了のシグナリングをしない (MSF §11.3)

MSF §11.3 はライブ配信の終了を次の 3 手順で伝えるとする。

1. 全 track に status code `0x2` (Track Ended) で終了を通知する
2. VOD 化する場合は `isLive=false` と `trackDuration` を入れた独立カタログを公開する
3. 恒久終了の場合は `isComplete=true` と空の `tracks` を持つ独立カタログを公開する

moq-pub は終了時に `PUBLISH_DONE(GOING_AWAY = 0x4)` を送るだけである
(`examples/moq-pub/src/pipeline.rs:1570-1601`)。`PUBLISH_DONE_TRACK_ENDED` は
example から使われず、catalog は `is_complete: false` / `isLive: true` 固定で
`trackDuration` も設定しない (`examples/moq-pub/src/catalog.rs:154-205`)。
購読側は配信が正常終了したことを判別できない。
なお MSF §11.3 は `SUBSCRIBE_DONE` と書くが、draft-22 で `PUBLISH_DONE` に改名されている
(コードは `src/error.rs:258` の `PUBLISH_DONE_TRACK_ENDED = 0x2`)。

### 5.2 PUBLISH に Message Parameters を載せられない

`examples/tokio-moq/src/moqt_client.rs:1436-1462` の `publish_track` は
`MessageParameters` を引数に取らず、内部で空のパラメータを渡す。
ライブラリ側は `Session::send_publish(..., parameters, ...)` で任意のパラメータを受け付ける
(`src/session/subscription/send.rs:254-261`) ため、transport 層のラッパーの制約である。
結果として `EXPIRES` (§9.20.16)・`SUBSCRIBER_PRIORITY` (§9.20.7)・
Delivery Timeout (§9.20.3 / §9.20.4)・`FORWARD` (§9.20.18)・`GROUP_ORDER` (§9.20.8) を
PUBLISH に載せられない。

### 5.3 配送タイムアウトを載せていない

`OBJECT_DELIVERY_TIMEOUT` / `SUBGROUP_DELIVERY_TIMEOUT` (§5.2、§10.1、§10.2) を
Track Property としても Object Property としても設定していない
(`examples/moq-pub/src/pipeline.rs:236-256` で映像は `DYNAMIC_GROUPS` のみ、
音声と catalog は `TrackProperties::new()`)。値が 0 (無設定) のときリレーは
期限切れの Object を破棄しないため、ライブ配信で遅延が伸びる原因になる。
ライブラリ側は購読者からのタイムアウトを反映する実装がある
(`src/session/subscription/dispatch.rs:599-690`) が、publisher 自身の値は 0 のままである。

### 5.4 優先度が 128 固定で、CLI から変えられない

publisher priority は `const DEFAULT_PUBLISHER_PRIORITY: u8 = 128;`
(`examples/moq-pub/src/pipeline.rs:121`) を subgroup header と datagram の双方へ
そのまま渡している (`:917`、`:1001`、`:1017`、`:1187`、`:1206`)。
`DEFAULT_PUBLISHER_PRIORITY` Track Property (§10.4) も `SUBSCRIBER_PRIORITY` も設定せず、
CLI に優先度のオプションも無い。音声と映像が同じ優先度になり、
輻輳時に音声を優先する制御ができない (§5.1.3)。

### 5.5 Track Properties の大半を告知していない

PUBLISH に載せる Track Property は `DYNAMIC_GROUPS` など一部だけで、以下は
`examples/` に出現しない (`MAX_CACHE_DURATION` §10.3、`DEFAULT_PUBLISHER_PRIORITY` §10.4、
`DEFAULT_PUBLISHER_GROUP_ORDER` §10.5、`IMMUTABLE_PROPERTIES` §10.7、
`EXPIRES` §9.20.16)。保持期間や既定値を宣言しないため、subscriber は
いつまで取得できるかを判断できない。

### 5.6 Range Filters を有効にできない

`MAX_FILTER_RANGES` を SETUP で宣言していない
(`examples/tokio-moq/src/lib.rs:42-82` が送るのは `PATH` / `AUTHORITY` /
`MOQT_IMPLEMENTATION` / `AUTHORIZATION_TOKEN` のみ)。
§9.1.6 は既定 0 のとき「peer は Range filter パラメータを送ってはならない (MUST NOT)」と
定めるため、購読者は `SUBGROUP_FILTER` / `OBJECTID_FILTER` / `PRIORITY_FILTER` /
`OBJECT_PROPERTY_FILTER` を使えない。

### 5.7 TRACK_STATUS を拒否する

moq-pub の peer request 処理は SUBSCRIBE と FETCH だけを扱い、それ以外は
`REQUEST_NOT_SUPPORTED` で拒否する (`examples/moq-pub/src/pipeline.rs:1911-1916`)。
ライブラリは TRACK_STATUS の受信と応答に対応している
(`src/session/track_status.rs`) ため example 側の実装漏れである。
§9.13 は SUBSCRIBE_OK と同じ Track Properties を持つ応答を求めるが、
transport 層の `send_request_ok` も Track Properties を渡せない
(`examples/tokio-moq/src/moqt_client.rs:1697-1703`)。

### 5.8 メディアの FETCH に応答しない

FETCH に応答するのは catalog track だけで、映像 / 音声の FETCH は `DOES_NOT_EXIST` で
拒否する (`examples/moq-pub/src/pipeline.rs:1825-1910`)。fill fetch も catalog のみで、
映像 / 音声は reset する (`pipeline.rs:1303-1367`)。
draft-22 §3.2 は FETCH を「過去の Object を取得する」機構として定義しており、
publisher が保持していれば応答できる。現状はメディアを保持もしないため、
subscriber は途中参加より前の Group を取得できない。

### 5.9 REQUEST_UPDATE の扱いが限定的

受信する REQUEST_UPDATE (`examples/moq-pub/src/pipeline.rs:1368-1414`) は
常に `REQUEST_OK` を返し、実質的に扱うのは `NEW_GROUP_REQUEST` だけである。
フィルタ変更と Forward (pause) はライブラリが反映する
(`src/session/subscription/recv.rs`、`src/session/subscription/validation.rs`) が、
priority の変更は購読状態に保存されるだけで送信に反映されず、
`EXPIRES` の延長は `REQUEST_UPDATE_OK` に載せる経路が無い
(`examples/tokio-moq/src/moqt_client.rs:1690-1703` が空パラメータ固定)。
また pause 中もエンコーダは止めず、フィルタ不通過として捨てるだけである。

### 5.10 複数トラック / 階層符号化を出せない

1 つの namespace に `--track-name` (既定 `video`) の 1 本と `audio` の 1 本、
catalog の 1 本しか publish しない (`examples/moq-pub/src/pipeline.rs:474-506`)。
MSF §5.6.2 (simulcast) / §5.6.3 (SVC) や LOC §4.4〜§4.6 が想定する
`altGroup` / `renderGroup` を使った複数品質の同時配信、`temporalId` / `spatialId` の
告知ができない。品質の切り替えには再起動が要る。

### 5.11 メディア入力の制約

- 音声は 48 kHz / モノラル固定 (`examples/moq-pub/src/pipeline.rs:126-132`、
  `audio_capture.rs:29-32`)。CLI にチャンネル指定が無く、ステレオ入力は
  libopus のダウンミックスに任せる (`decoder/opus.rs:17-22`)
- `--input-mp4` のパススルーは B フレームを含む MP4 を拒否する (`mp4.rs:9,143,179-186`)
- HDR / 色情報の扱いが無い。MSF の `displayWidth` / `displayHeight` も設定しない
- 解像度は起動時に確定し、配信中の変更ができない (`pipeline.rs:511-560`)

### 5.12 catalog の任意フィールドを出していない

`examples/moq-pub/src/catalog.rs:154-189` が設定するのは name / namespace /
packaging / role / isLive / codec / width / height / framerate / bitrate /
maxGopDuration / maxGroupDuration / targetLatency / renderGroup / samplerate /
channelConfig / authInfo である。必須と条件付き必須は満たすが、
`timescale` (§5.2.21) / `mimeType` (§5.2.19) / `avgBitrate` (§5.2.23) /
`displayWidth` / `displayHeight` (§5.2.30 / .31) / `lang` (§5.2.32) /
`label` (§5.2.10) / `altGroup` (§5.2.12) / `depends` (§5.2.14) /
`template` (§5.2.15) / `temporalId` / `spatialId` (§5.2.16 / .17) /
`trackDuration` (§5.2.35) / `accessibility` (§5.2.44) は未設定である。
購読側の言語選択や表示制御の材料が無い。

### 5.13 逆方向 track (Log / Metrics) を告知しない

MSF §5.1.5 の `publishTracks` を出さないため、subscriber 側から Log / Metrics を
publish する経路が無い (§5.6.16、§10)。

## 6. subscriber (moq-sub) に足りない機能

### 6.1 FETCH を使わない (シーク・巻き戻し・欠落回収ができない)

`examples/moq-sub/src/pipeline.rs:2411` に「本 example は FETCH を発行しない」とあり、
`MoqtClient` にも発行 API が無い。カタログは publisher が開く fill fetch stream と
SUBSCRIBE の 2 経路で受けるが (`pipeline.rs:1657-1801`)、メディアの fill は要求せず、
届いた fill fetch stream は読み捨てる (`pipeline.rs:2411-2417`)。
そのため次ができない。

- ライブ配信の途中参加時に、キーフレームまで遡って表示を開始する (§3.5)
- 受信が途切れた区間を後から埋める (§3.2.2 のギャップ)
- VOD (MSF §5.6.7) の任意位置からの再生

### 6.2 トラック選択が「最初に見つかった 1 本」で固定

`examples/moq-sub/src/pipeline.rs:1812-1831` は catalog の並び順で最初に一致した
映像コーデック / 音声コーデックのトラックを選ぶ。`--track-name` のような選択肢が無く、
MSF §5.2.6 (role) / §5.2.10 (label) / §5.2.12 (altGroup) / §5.2.32 (lang) による
選択も切替もできない。多言語・複数品質のカタログでは意図しない 1 本を選びうる。

### 6.3 catalog の delta update で追加された track を購読しない

`examples/moq-sub/src/pipeline.rs:2086-2115` は適用結果をログに出すだけで、
track の追加 (§5.3 の `add` / `clone`) に追随して SUBSCRIBE を送らない。
削除 (`remove`) に追随した購読終了もしない。カタログが動的に増える配信では、
後から増えた track を永久に受信できない。

### 6.4 `authInfo` を参照せず、変数置換もしない

MSF §11.4.1 は「購読者は track ごとに `authInfo` を見て認可要件を判断する」と定める。
moq-sub は `authInfo` を参照せず (grep で 0 件)、track ごとの認可要件を判定しない。
さらに moq-pub は `authInfo` の値に `"%c4m%"` という変数参照を書く
(`examples/moq-pub/src/catalog.rs:31`、§5.2.43) が、moq-sub は
`MsfCatalogDocument::decode` を直接呼び (`examples/moq-sub/src/catalog.rs:32-38`)、
`msf::resolve_catalog_variables` (§5.4) を通さない。
このため example 同士でも認可付き配信の受け渡しが成立しない。

### 6.5 `packaging` と `isLive` を見ない

トラックを選ぶ条件は codec の一致だけで、`packaging` (§5.2.4) と `isLive` (§5.2.7) を
判定していない。`loc` 以外の packaging (`mediatimeline` / `eventtimeline` / `moqlog` /
`moqmetrics`) の track を購読しても LOC として解釈するため、受信後にデコードで失敗する。
`isLive=false` の track も live と同じ経路で購読する (4.2 も参照)。

### 6.6 GOAWAY によるセッション移行

両 example は control stream の GOAWAY を受けると、New Session URI (空なら現在の URI) へ
バックグラウンドで新 session を確立し、Established な購読 / 配信を張り直してから旧 session を
NO_ERROR で閉じる。接続は GOAWAY の Timeout を期限に指数バックオフで再試行し、認可トークンは
同一 origin のときだけ引き継ぐ。旧 session は移行の完了を待たずに timeout 分 sleep することはなく、
StopSending / PUBLISH_DONE のあとすぐに閉じる。移行先の確立や購読の張り直しが一時的に失敗した
場合は期限が残っている限り接続からやり直す。request stream の GOAWAY は session を維持し、
空 URI (subscriber は現在と同じ URL を指定された場合を含む) なら当該 request を現在の
session で再発行する。

残る制約:

- request stream GOAWAY の非空 URI による単一 request 移行は未対応である。指定 URI の session を
  別に確立し、track 単位に data plane / datagram / alias / peer request を振り分ける設計が必要に
  なる。現状は移行せず warn ログを残す。
- subscriber は移行後の catalog 購読で届く fill fetch stream を `CatalogState` へ適用する。
  publisher が fill に応答しない場合は次の catalog publish まで完全な catalog が届かない
  (外部要因)。
- 音声は移行境界を最後に受信した Location にして Absolute Start で再購読する。移行先が同じ
  publisher の続きとして TIMESTAMP を進める前提であり、別 publisher へ移って TIMESTAMP が
  巻き戻る場合は live 音声が stale として落ちる。
- 移行先の c4m が変わった場合、publisher は catalog の authInfo を新しいトークンで再構築する。
- 移行先がまた GOAWAY を返す連鎖は `MigrationLimiter` (10 分で 5 回) で制限する。
- transport (HTTP/3 / HTTP/2) の GOAWAY と WT_DRAIN_SESSION は `WtSessionState::Draining` として
  観測し、現在 URL への移行を開始する。QUIC / QMux は drain 通知が無いため MOQT GOAWAY 起点のみ。
- 期限を過ぎても移行できない場合は従来どおり終了する。

### 6.7 REQUEST_ERROR の Redirect に追随しない

`Redirect` structure 付きの `REQUEST_ERROR` (`0x34`、§9.4.1) を example は解釈しない。
session は `redirect` と `retry_interval` をイベントに載せる
(`src/session/types.rs:283-289`) が、transport 層は `(error_code, reason)` だけを記録して
`redirect` を捨てる (`examples/tokio-moq/src/moqt_client.rs:2676-2690`)。
リダイレクト先へ購読を張り直す経路が無い。

### 6.8 TRACK_STATUS を使わない

購読前にトラックの有無や最新 Object を確認する §9.13 の経路を持たない。
`--no-video` / `--no-audio` は catalog の内容だけで判断する。

### 6.9 REQUEST_UPDATE が固定 priority の 1 回だけ

REQUEST_UPDATE の送信は `examples/moq-sub/src/pipeline.rs:784-794` の 1 箇所だけで、
固定値 192 の `SUBSCRIBER_PRIORITY` を送る (コメント上もサンプル)。§9.5 が購読で
許す更新のうち、Forward (pause / 再開、§3.1.1)・Group Order・Location Filter・
Range Filter・Delivery Timeout はいずれも送らない。REQUEST_UPDATE_OK も
catch-all で読み捨てる (`pipeline.rs:1280`)。`FORWARD=0` による一時停止ができない。

購読中の `NEW_GROUP_REQUEST` (§9.20.19、§3.5.1) も送れない。session は
`DYNAMIC_GROUPS` を受けて保持する (`src/session/subscription/recv.rs:638`) が、
`MoqtClient` に公開 accessor が無く (`examples/tokio-moq/src/moqt_client.rs:261-271`)、
購読確立後に新しい Group を要求する経路が API として存在しない。

### 6.10 SUBSCRIBE に載せるパラメータが 3 種類しかない

moq-sub が実際に載せるのは catalog の `LOCATION_FILTER` + `FILL_PARAMETERS`
(`pipeline.rs:1632-1650`)、映像の `NEW_GROUP_REQUEST=0` (`pipeline.rs:482-490`)、
映像への REQUEST_UPDATE の `SUBSCRIBER_PRIORITY=192` (`pipeline.rs:784-794`) である。
音声の SUBSCRIBE はパラメータ無し (`pipeline.rs:817-819`)。以下は未使用である。

- メディアの `LOCATION_FILTER` (§9.20.9、§3.3.1)。開始位置を指定できないため、
  Group 途中で参加するとキーフレームまで破棄する (`pipeline.rs:3026-3043`)
- Delivery Timeout (§9.20.3 / §9.20.4)。購読側から許容時間を指定できない
- `GROUP_ORDER` (§9.20.8)
- Range Filters (§9.20.10〜§9.20.13、§3.3.2)。そもそも `MAX_FILTER_RANGES` が
  宣言されていないため送れない (5.6)
- `INCLUDE_PROPERTIES` (§9.20.21)
- `FILL_TIMEOUT` (§9.20.5、§9.20.15)
- `EXPIRES` (§9.20.16)。`SubscribeResult` は `largest_object` しか公開せず
  (`examples/tokio-moq/src/moqt_client.rs:261-271`)、期限延長の要求ができない

### 6.11 Object Status / PUBLISH_DONE / Largest Object を判断に使わない

- Object Status (`0x3` End of Group / `0x4` End of Track、§11.1.1) の意味を解釈しない。
  Subgroup / Fetch stream では status を見ずに `payload_length == 0` を一律 skip し
  (`pipeline.rs:2948-2950`、音声は `:3435-3437`)、datagram は status 付きを
  `Ignore` にするだけである (`pipeline.rs:3359`)。デコーダは status を保持している
  (`src/stream/decoder.rs:26-42`) ため、Group / Track の終端を PUBLISH_DONE 頼みでしか
  検知できない
- PUBLISH_DONE (§9.9) はログのみ (`pipeline.rs:1214-1223`)。`stream_count` の突き合わせや
  §12.4 の status code 別の扱いをしない
- Largest Object (§3.1.4、§9.20.17) はログのみ (`pipeline.rs:667-672`)。
  fill range の判断に使わない

### 6.12 MSF の Timeline・範囲指定・buffers を使わない

Media / Event Timeline (§7 / §8)、`msf::uri` の範囲パラメータ (§11.1.1)、
`buffers` / `maxGopDuration` / `maxGroupDuration` (§5.2.9 / §5.2.24 / §5.2.25) は
いずれも example から使われない。Timeline を使った A/V 同期と任意位置再生、
バッファ量の指示に基づく再生制御に対応しない。
ライブラリにも §7.3 / §8.3 の「各 Group の最初の Object は独立、以降は差分」を
蓄積する状態型が無い (payload の encode / decode のみ)。

### 6.13 再生面の機能が無い

シーク、一時停止、音量、トラック切替、字幕、複数映像の同時表示、帯域適応、
統計表示のいずれも無い (`examples/moq-sub/src/main.rs` に該当する処理が無く、
`poll_events()` でウィンドウクローズだけを見る)。映像は codec 一致の 1 本のみである。
音声出力は `default` / `none` のみ (`cli.rs:45-58`)、録画は MP4 のみで
音声は Opus に限る (AAC は警告して音声を録画しない)。

### 6.14 datagram は音声しか扱わない

datagram で届いた Object を処理するのは音声だけで (`pipeline.rs:3356-3368`)、
映像と catalog の datagram は 1 回警告して捨てる (`pipeline.rs:1091-1097`)。
§11.2 は任意の track に datagram を許すため、publisher が datagram 配送を選ぶと追随できない。

## 7. transport 層 (examples/tokio-moq) の制約

`examples/tokio-moq/README.md` に明記されている内容の再掲を含む。

- `wt-h3` は `RESET_STREAM_AT` を送出できない (0094)。request stream の cancel や
  送信中の data stream の中断が MoQ リレーによっては成立しない
- `wt-h2` の reset は `WT_RESET_STREAM` capsule に依存し、リレーの対応が要る。
  また §6.2 により datagram が reliable / ordered になるため、音声を datagram で
  送る既定の遅延特性が変わる
- `qmux` は QMux (draft-ietf-quic-qmux-02) 経由であり、MOQT 側に QMux 用の ALPN 登録が
  無いため native QUIC と同じ `moqt-22` を広告する。QMux 自身の §8.1 は「QUIC マッピングと
  異なる識別子」を MUST で求めるため、この選択は同節から外れる (interop を優先した)。
  両 draft が決着したら見直す
- `qmux` の DATAGRAM は経路での損失を下位の TCP が再送するため起きないが、TCP が再送した
  分だけ遅延が伸びる。加えて QMux はアプリへ速やかに配送できない DATAGRAM の破棄を許すため
  (draft-ietf-quic-qmux-02 Section 6)、受信側のバッファが溢れると欠落しうる。また
  `RESET_STREAM_AT` を実装しないため、reset の `reliable_size` は常に `None` になる
- QMux 上の MOQT を扱う individual draft
  (`refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt`) は MOQT draft-16 を前提としており、
  同 draft の control stream (双方向 stream 0) は draft-ietf-moq-transport-22 §6.4.1
  (control stream は単方向) と矛盾する。本実装は QUIC version 1 との 1 対 1 の対応を採る
- `--cert` を省略すると証明書検証をスキップする。mTLS (§15.3.1) には対応しない
- 0-RTT (§6.3.1) と再接続 / セッション再開は未対応 (`rg 'early_data|0-rtt|resumption' examples/` が 0 件)
- 無通信時の切断対策が無い。§15.6.1 が挙げる 3 つの選択肢 (transport の keep-alive、
  定期 control message、切断後の再接続) のいずれも実装していない
- STOP_SENDING の error code を指定できない (`pipeline.rs:1389-1393`、§12.5 は SHOULD)
- DPoP proof (`cnf` / `catdpop` 付きトークン) を送らない (C4M §3.2.2)
- Extension Negotiation (§6.3.2) の枠組みが無い。拡張を使わないので現状の実害は無い
- WebTransport 経由の datagram 受信は現行コードでは `take_buffered_datagrams` 経由で
  実装されている (`examples/tokio-moq/src/transport.rs:431-438`)。
  `issues/pending/0010` は削除済みの `examples/moq-transport` を指すため、
  現行で再現するかは要確認である

## 8. 付録: ドキュメントと実装の齟齬

実装漏れではないが、調査中に見つかった記述のずれを記す。

- moq-pub のフラグ名: 実際は `--audio-subgroup` (既定が datagram) だが、
  `examples/moq-pub/src/error.rs:72` と `:213` のメッセージは
  `--audio-datagram` と案内している
- `examples/moq-sub/README.md:76` は「moq-pub は `--audio-datagram` でこの配送を選べる」と
  書くが、そのようなフラグは無い (既定が datagram で、subgroup にするのが `--audio-subgroup`)。
  `examples/moq-sub/src/pipeline.rs:4775` のコメントも同じ
- MSF §11.3 は `SUBSCRIBE_DONE` と書くが、draft-22 で `PUBLISH_DONE` に改名されている。
  実装は改名後に追従している

## 9. 優先度の目安

利用者に直接見える欠落から順に並べる。

1. request stream GOAWAY の非空 URI による単一 request 移行 (6.6) — 2 session の併走が必要
2. メディアの FETCH 不在 (5.8 / 6.1) — シーク、巻き戻し、欠落回収ができない
3. 配信終了のシグナリング (5.1) — 正常終了・VOD 化・恒久終了を伝えられない (MSF §11.3)
4. authInfo と変数置換 (6.4) — 認可付き配信が example 同士でも成立しない
5. catalog delta で増えた track への追随 (6.3) — 動的な配信構成に追随できない
6. Object Status と PUBLISH_DONE の未解釈 (6.11) — トラック終端を正しく検知できない
7. 配送タイムアウトの未設定 (5.3) — ライブ遅延が伸びる
8. トラック選択 (6.2) と `isLive=false` の扱い (4.2 / 4.3) — 意図しないトラックを選ぶ
9. Object 側 `IMMUTABLE_PROPERTIES` の探索 (4.1) — §10.7 MUST 未達の疑い
10. `END_OF_GROUP` (3 章) / Track Properties / `EXPIRES` (5.2〜5.5) — リレーと購読者への情報不足
11. Range Filters の有効化 (5.6 / 6.10)、TRACK_STATUS (5.7 / 6.8)、Redirect (6.7)、pause (6.9)
12. STOP_SENDING 時の open stream の reset (4.7) — アプリが閉じ忘れると cleanup が進まない
13. Timeline / 変数置換 / Metrics (5.13、6.12)、複数トラック配信 (5.10)
14. extension negotiation の API 非公開 (4.8)、PUBLISH_OK / FETCH_OK 前の Object 送信 (3 章、いずれも MAY)

## 10. 要確認

- Object 側 `IMMUTABLE_PROPERTIES` の探索 (4.1) を `issues/0125` の射程に含めるか
- LOC §2.2 (Private Properties) の実害 (`src/loc.rs:12-13` は application 責務としている)
- `issues/pending/0010` (WebTransport の datagram 受信) が現行コードで再現するか
- Video Frame Marking の D / B / T / S ビットを使う予定があるか
  (現状は I ビットのみ、`examples/moq-sub/src/pipeline.rs:3143-3165`)
- カメラ経路のエンコーダ (Video Toolbox / libaom / libvpx) が B フレームを出力しうるか
  (パススルーは B フレーム入力を明示的に拒否する)
- 0-RTT: rustls の `enable_early_data` 既定は false だが s2n-quic 側の既定は未確認
- STOP_SENDING 受信時の subgroup / FETCH uni stream の reset (4.7) を
  アプリ責務と解釈しているか (`src/session/types.rs:750-756` は設計として明記している)
- moqt URI のパースをライブラリに置くか example に置くか (§6.1、URL 系の issue は
  example 側で closed になっている)
- 送信パラメータ検証の残存ギャップ (`src/session/subscription/send.rs:126` / `:307` /
  `:323` / `:927` のコメント) を encode 前に閉じるか
- リレー経由でない直接 subscriber の受け入れは、moq-pub が client 専用 (待ち受け API 無し) の
  ため不可。要件次第で判断が必要
- MSF §7.1 は payload 文書が explicit 形式と template 形式の 2 つを併用してよい (MAY) と
  するが、payload 内の template 形式を定義する節が無い (draft 側の記載不足)
- MSF §5.1.6 の delta に `isComplete` が混ざった場合、library は黙って無視する。
  「restricted set of fields」の解釈次第で拒否すべきか
- C4M §2.2 の `moqt-reval` を複合クレーム内に置いたトークンの well-formed 性拒否は未実装
  (composite claims の draft が `refs/` に無い)
