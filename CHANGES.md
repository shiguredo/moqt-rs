# 変更履歴

- UPDATE
  - 後方互換がある変更
- ADD
  - 後方互換がある追加
- CHANGE
  - 後方互換のない変更
- FIX
  - バグ修正

## develop

- [CHANGE] `ObjectDatagram` と `send_object_datagram` の `properties_data` を Properties Length varint 込みの生バイト列に統一し、Properties Length = 0 と宣言長不一致を拒否する。これにより moqt-publisher の datagram_writer が Properties Length を二重に書かなくなる
  - @voluntas
- [CHANGE] `send_subgroup_object` / `send_object_datagram` の戻り値を `SendRequestError` に変更し、`send_publish` のフィルタ不通過も `SendRequestError::LocalFilterMismatch` にする。wire コードを取る公開 API はローカル専用コードを各レジストリの `*_INTERNAL_ERROR` に置換する
  - @voluntas
- [CHANGE] `recv_subgroup_object` の戻り値を `TrackDataAcceptance` に変更し、受信 subgroup Object にも Object 単位フィルタを再適用する。フィルタ不通過は `FilteredOut`、キャンセル済み subscription への不要 Object は `Discarded` として破棄する
  - @voluntas
- [ADD] peer が SETUP で宣言した MAX_AUTH_TOKEN_CACHE_SIZE を取得する `Session::peer_max_auth_token_cache_size()` を追加する
  - @voluntas
- [FIX] AUTHORIZATION_TOKEN と Setup Option Bytes の encode に値長 2^16-1 バイトの上限検証を追加する
  - @voluntas
- [FIX] subscriber 側の Stream Count 集計と `cleanup_ready` の open stream 判定に受信 fill fetch stream を含め、PUBLISH_DONE 受信後に遅延到着した fill fetch stream も late-opening stream として受理する
  - @voluntas
- [FIX] SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の REQUEST_UPDATE で送信した TRACK_NAMESPACE_PREFIX を REQUEST_OK 受信後に反映し、送信前の overlap 検査は確定待ちを考慮した実効 prefix で行う。SUBSCRIBE_TRACKS は確定待ちの間も旧 prefix と確定待ち prefix の両方で PUBLISH を紐付ける
  - @voluntas
- [FIX] FETCH の INVALID_RANGE 判定で同一 Track の全 publisher 役 subscription の観測 Largest Object の最大値を使う
  - @voluntas
- [FIX] REQUEST_UPDATE の Range Filter 拒否で自側 publisher の subscription を PUBLISH_DONE(UPDATE_FAILED) で終端し、Pending / Terminated への REQUEST_UPDATE は PROTOCOL_VIOLATION で閉じる
  - @voluntas
- [FIX] FetchStreamEncoder が先頭の Datagram 起源 Object をエンコードできるようにし、Datagram 起源 Object の Subgroup ID をデコード結果 (0) と一致させる
  - @voluntas
- [FIX] DYNAMIC_GROUPS=1 でない Track への REQUEST_UPDATE に NEW_GROUP_REQUEST を含めると送信前に拒否し、同一 REQUEST_UPDATE の他パラメータをローカルに適用しない
  - @voluntas
- [FIX] SUBSCRIBE_TRACKS の bidi stream 終端後の PUBLISH の bidi stream 終端でセッションを閉じないようにし、RequestTerminated の kind を実際の要求種別に合わせる
  - @voluntas
- [FIX] MSF の isLive=false で targetLatency / buffers をエンコードしないようにする
  - @voluntas
- [FIX] TRACK_STATUS_OK を FIN で送信し、公開済み Track の LARGEST_OBJECT を自動注入する
  - @voluntas
- [FIX] PUBLISH_DONE の Stream Count で 0 stream 時に sentinel を送れないようにする
  - @voluntas
- [FIX] MsfCatalog::apply_delta の add 経路でトラックの MUST 制約を検証する
  - @voluntas
- [FIX] SubgroupObject の encode を書き込み前に検証し、不正 status の部分書き込みと Properties Length 欠落を防止する
  - @voluntas
- [FIX] `.session` 名前空間の非空トラック名への FETCH / TRACK_STATUS を `DOES_NOT_EXIST` で拒否する
  - @voluntas
- [FIX] FetchStreamObject の encode で空スライス (`Some(&[])`) の properties を拒否し、Properties Length 欠落の不正ワイヤ生成を防止する
  - @voluntas
- [FIX] `SubgroupObject` / `FetchStreamObject` の encode で Properties Length と実データ長の不一致を `ProtocolViolation` として拒否し、不正ワイヤ生成を防止する
  - @voluntas
- [FIX] moqt-publisher の SubgroupWriter が最初に送信する Object の時点で FIRST_OBJECT を確定し、フィルタ不通過で省略した Object がある場合は FIN ではなく reset で終端する
  - @voluntas
- [FIX] moqt-transport の MoqtClient::stop_sending が bidi request stream に実際の STOP_SENDING を送出するようにする
  - @voluntas
- [FIX] moqt-subscriber の run_raw_player が raw_player の初期化・プレイヤー生成・再生開始の失敗を panic ではなくエラーとして扱い、終了コード 1 で終了するようにする
  - @voluntas

### misc

- [UPDATE] MSF の track / cloneTrack の JSON メンバー書き出しと共通検証を共通化する
  - @voluntas
- [UPDATE] session の request 種別変換を 1 箇所に集約し、購読索引の追加・削除を単一経路化する
  - @voluntas
- [UPDATE] コード内 doc コメントの実装との不整合を修正する
  - @voluntas
- [UPDATE] 未解決の intra-doc link を修正する
  - @voluntas
- [UPDATE] no_std ビルドと rustdoc 検査を CI に追加する
  - @voluntas
- [UPDATE] fuzz_session を client / server 両対応にし、送信 API を操作列に追加する
  - @voluntas
