# moq-sub がカタログ再送の subgroup header を二重登録し 30 秒後にセッションを閉じる

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moq-sub-catalog-header-reregister
- Polished: 2026-10-13

## 目的

moq-pub を起動したあと moq-sub で受信すると、カタログの再送が届いた約 30 秒後に subscriber が自分からセッションを閉じる。映像は直前まで届いている。配信と受信は Ctrl+C まで続ける想定であり、この終了は起きてはならない。

## 現状

再現手順は次のとおり。接続先の MOQT relay は別途用意する。

```bash
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example
```

実測では、映像を描画している途中で次の警告が出た。

```text
WARN moq_sub::pipeline: Stream #7: failed to read catalog stream: failed to register subgroup header: internal error: recv_subgroup_header: session error 0x3: subgroup header already received for stream id
```

警告の 29.98 秒後に subscriber が自分から閉じた。`0x12` は 10 進数の 18 であり、QUIC の `application::Error(18) by the local endpoint` は同じ終了である。

```text
WARN moq_sub::pipeline: Session closed: 0x12 data stream timeout expired
```

その時点の描画は 1246 video frames / 0 audio chunks、受信ストリームは 22 本だった。表示待ち 6 フレームが残っており、映像はその直前まで届いていた。音声チャンクは 0 のままだったが、タイムアウトの起点は上記のカタログ警告と一致する。

処理の流れは次のとおり。

- `examples/moq-pub/src/pipeline.rs` の `CATALOG_REPUBLISH_INTERVAL` (30 秒) で、独立したカタログを新しい Group として送り直す。起動時に解決したあとの購読へ届くのがこの再送である。
- `examples/moq-sub/src/pipeline.rs` の `handle_stream_body` は、Subgroup のヘッダを `register_subgroup_header` で Session へ登録する。`Session::recv_subgroup_header` はここで stream を `IncomingDataStream::Subgroup` にし、`data_stream_last_activity_ms` を更新する。
- トラック種別が `TrackKind::Catalog` のとき、同じ関数は続けて `read_catalog_subgroup_stream` を呼ぶ。この関数は新しい `SubgroupStreamDecoder` でヘッダを読み直し、`recv_subgroup_header` をもう一度呼ぶ。
- 2 回目は `src/session/data.rs` の `Session::recv_subgroup_header` が `SESSION_PROTOCOL_VIOLATION` (`0x3`、`"subgroup header already received for stream id"`) を返す。この分岐は `Session::fail` を呼ばないため、セッションは Established のままである。
- `read_catalog_subgroup_stream` はエラーを返すだけで、`drain_registered_stream_to_end` も `DataPlaneHandle::send_data_stream_stop_sending` も呼ばない。呼び出し側の `handle_stream_body` も警告を出して戻る。`spawn_stream_task` は stream を drop するだけで `recv_data_stream_closed` を呼ばない。
- 最初の登録で入った activity が更新されないまま 30 秒経つ。`examples/moq-sub/src/pipeline.rs` の `run` が `set_data_stream_timeout_ms(Some(30000))` しており、`Session::tick` が `SESSION_DATA_STREAM_TIMEOUT` (`0x12`) でセッション全体を閉じる。

起動時のカタログ取得 (`receive_catalog`) は、カタログが fill fetch stream で届けば `read_catalog_fetch_stream`、subgroup
stream で届けば `read_catalog_subgroup_stream` を呼ぶ。後者の分岐は事前の `register_subgroup_header` を持たないため、
ヘッダ登録は 1 回で済む。二重登録が起きるのは、`handle_stream_body` が登録済みの stream に対して同じ関数を再利用した
ときである。

この経路では Object のループに到達しないため、再送されたカタログの内容は `CatalogState` に反映されない。data stream
timeout を無効にした運用では、カタログが起動時のまま固定される。

再現手順では subscriber の音声は既定で有効であり、音声は 1 object ごとに新しい stream を開くため、音声が流れている
run では受信 stream 数は数千になる。上記の実測値 (22 本 / 音声チャンク 0) は音声が届いていない run のものである。

同じファイルの fill fetch 失敗経路は、読み切れなかった stream を `send_data_stream_stop_sending` で Session の会計から外す。コメントは「送らないと受信 stream が残り、data stream timeout でセッションが落ちる」と書いている。`Session::send_data_stream_stop_sending` は `data_stream_last_activity_ms` から stream id を除去する。カタログ再送の失敗経路にはこの後始末が無い。

`0x12` で終わる症状は closed の `issues/closed/0101-bug-subscriber-recv-loop-stall.md` にもある。あちらは relay の経路 MTU 超過で転送が止まったあとのタイムアウトであり、本 issue の二重登録とは別である。

## 設計方針

- ヘッダ登録と Object のループを分ける。`read_catalog_objects(stream, data_plane, stream_id, decoder, header) ->
  Result<Vec<CatalogObject>>` を切り出し、`read_catalog_subgroup_stream` (ヘッダ登録と track alias の判定) と
  `handle_stream_body` の `TrackKind::Catalog` 分岐の両方から呼ぶ。`SubgroupStreamDecoder` は復号済みのヘッダを
  保持しないため、`header.group_id` (Location 用) と `header.track_alias` は呼び出し側から渡す。
- `handle_stream_body` の `TrackKind::Catalog` は、`register_subgroup_header` 済みの decoder から `read_catalog_objects` を
  呼ぶ。ヘッダを含む `buf` は渡さない (渡すとヘッダを復号し直し、同じ二重登録の温床になる)。`read_catalog_subgroup_stream`
  は `receive_catalog` が未登録の stream に対してヘッダ登録から行う入口のままにする。
- カタログの読み取りに失敗したときは、fill fetch の失敗経路と同じく `DataPlaneHandle::send_data_stream_stop_sending` で
  監視対象から外し、stream を drop する (読み切れなかった stream を Session の会計から外すと data stream timeout の
  対象から外れる。draft-ietf-moq-transport-22 §12.2 (Session Termination Codes) は、open な Subgroup stream で新しい
  object header を待って timeout した場合に STOP_SENDING を送るか subscription を終了してもよい (MAY) と定める)。
  読み切れたときは、既存の `receive_registered_stream_data` が FIN で `recv_data_stream_closed` する経路に乗せる。
- `Session::recv_subgroup_header` の「登録済みなら拒否する」判定は変えない (2 回目を呼んでいるのは example 自身であり、
  仕様上 wire で同じ stream の 2 回目の SUBGROUP_HEADER は現れない)。

## 完了条件

- moq-pub のカタログ再送が届いても、`subgroup header already received for stream id` が出ず、その約 30 秒後に `0x12 data stream timeout expired` でセッションが閉じないこと。
- カタログの stream を登録した後に、同じ stream へ `recv_subgroup_header` を再登録しないこと (保留中の
  `UnknownTrackAlias` の再試行は対象外とし、登録に成功した後の再登録を禁じる)。
- カタログの読み取りに失敗した stream が原因で `DATA_STREAM_TIMEOUT` (0x12) が発火しないこと (読み切れなかった
  stream を Session の会計から外す)。
- 上記の Session 側の挙動が `tests/test_session/` の単体テストで固定されていること (同じ stream への 2 回目の
  `recv_subgroup_header` が `SESSION_PROTOCOL_VIOLATION` を返しても Established のままであること、
  `send_data_stream_stop_sending` の後に timeout が発火しないこと)。example の配線 (Catalog 分岐が再登録しないこと、
  失敗経路が STOP_SENDING を送ること) は実 relay と peer の data stream 送信が必要なため、0201・0214・0215・0216・0244 と
  同じく **I/O ハンドルが必要な配線部分はレビューで確認する**。
- ソースコードのコメントに issue 番号や issue への言及を書かないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
