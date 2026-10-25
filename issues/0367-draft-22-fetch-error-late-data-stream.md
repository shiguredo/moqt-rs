# FETCH_ERROR 受信後に遅延する in-flight FETCH data stream を破棄として受理する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-10-25

## 目的

FETCH_ERROR は draft-ietf-moq-transport-22 §1.5 (Response Message Naming) の REQUEST_ERROR の別名であり、FETCH への
応答として request stream 上を届く。§3.2 (Fetch) は "The publisher MUST send exactly one FETCH_OK or FETCH_ERROR in
response to a FETCH. The FETCH_OK or FETCH_ERROR can come at any time relative to object delivery." と応答の
タイミングを定める。§3.2.4 (Fetch State Management) は "Since a relay can start delivering FETCH
Objects from cache before determining the result of the request, some Objects could be received even if the FETCH
results in error." と明記する。つまり FETCH_ERROR を受信した後に in-flight の FETCH data stream (FETCH_HEADER /
Object / FIN) が届くのは正当な競合である。§3.2.4 はこの到着を明示の MUST で受容要求していないため、本 issue は
相互運用のための受信側方針として扱う。現状はこれを不正として `PROTOCOL_VIOLATION` でセッションを閉じるため、
準拠した publisher との間でセッションが落ちる。

## 現状

- `src/session/fetch.rs` の `Session::handle_err_for_fetch` は REQUEST_ERROR 受信時に `Pending` / `Established` の
  fetch を `Terminated` にする (`Pending` では `response_received` も立てる) が、遅延 data stream を許容する記録を
  残さない。`Terminated` の fetch への REQUEST_ERROR (data stream FIN 後の遅延応答) は状態を変えない。
- `src/session/data.rs` の `Session::recv_fetch_header` は fetch が `Terminated` のとき
  `Session::tolerates_late_data_stream` を確認し、許容されない場合は "FETCH_HEADER received for terminated fetch"
  で `fail` する。許容される場合は `register_discarded_stream(stream_id, None)` で破棄対象として登録する。
- `Session::recv_fetch_entry` は `IncomingDataStream::Discarded` への entry を "fetch entry received on discarded
  stream" で拒否し、保持集合 (`data_streams.discarded`) の stream id も `incoming` に無いため "unknown stream id"
  で拒否する。したがって header だけを破棄登録しても、続く Object でセッションが落ちる。
- `Session::tolerates_late_data_stream` は `rejected_request_ids` (自側が request を拒否・終端した場合) と request
  stream GOAWAY の ID だけを対象にしており、peer の REQUEST_ERROR で終端した fetch は含まれない。
- `Session::forget_fetch` は `remove_incoming_data_streams_for_request` を呼び、open 中の受信 stream は
  `tolerates_late_data_stream` が true のときだけ破棄対象の保持集合へ移す。peer の REQUEST_ERROR で終端した fetch
  は現状この判定を通らないため、`forget_fetch` 後の data stream は未知の request ID / stream id として
  `PROTOCOL_VIOLATION` になる。
- `tests/test_session/fetch/after_fin.rs` は data stream FIN → REQUEST_ERROR の順序を固定しているが、
  REQUEST_ERROR → data stream の順序は未検証である。`tests/test_session/fetch/unified.rs` の GOAWAY 遅延 header
  テストは header と FIN だけを通しており、Object の受容は検証していない。

## 設計方針

- `Session` に「peer の REQUEST_ERROR で終端した fetch の request ID を許容する集合」を専用フィールドとして追加する
  (`rejected_request_ids` には相乗りしない。`rejected_request_ids` は自側終端の意味を持ち、bidi close 通知で
  除去されるため、peer 起因の遅延 data stream 許容とは寿命が異なる)。各 entry は `peer_alias_retention_ms` を
  保持期間とする `DeadlineTimer` とし、`tick_discarded_data_stream_ids` と同じ tick で期限切れを掃除する。
  data stream が一度も届かない場合も保持期間の経過で消えるため、長寿命セッションでも件数は有界である。
- 記録するのは `handle_err_for_fetch` が `Pending` または `Established` の fetch を `Terminated` に遷移させた
  場合とする (`Established` は REQUEST_UPDATE 失敗応答の REQUEST_ERROR であり、publisher が送信途中の Object を
  持つ点は同じ)。既に `Terminated` の fetch への REQUEST_ERROR (FIN 後の遅延応答) は記録しない (2 本目の data
  stream を許容しないため)。
- `Session::tolerates_late_data_stream` に上記集合の有効 entry を OR で加える。これにより `forget_fetch` の
  `remove_incoming_data_streams_for_request` も同じ判定で受信 stream を保持集合へ移す。
- `Session::recv_fetch_header` は、許容対象の request ID に対して `register_discarded_stream(stream_id,
  Some(header.request_id))` で破棄対象として登録し、セッションを閉じない。request ID を保持するのは
  `forget_fetch` 後の終端吸収と、破棄対象 stream の帰属を維持するためである。
  fetch entry が `fetches` に無い (`forget_fetch` 済み) 場合、現状は fill fetch stream の解決へ落ちて
  "unknown request id" で閉じるため、許容集合に有効 entry がある request ID は fill 解決より前に判別し、
  同じく破棄対象として登録して `Ok` を返す。許容集合は fetch の request ID のみを持つため fill fetch
  stream の request ID と衝突しない。
- 対象 data stream の FIN / RESET_STREAM を観測した時点で、その stream に紐づく request ID を許容集合から
  除去する。同じ request ID で 2 本目の data stream を開くことは §3.2.1 (Fetch Object Delivery) の単一 stream
  制約に反するため、終端後は許容しない。終端が届かない場合は `peer_alias_retention_ms` の期限で消える。
  1 本目の data stream が開いている間に同じ request ID の 2 本目の FETCH_HEADER が届いた場合は、
  `Session::has_other_fetch_stream` で拒否する (同関数は `IncomingDataStream::Discarded` も同じ request ID の
  stream として数えるよう更新する)。
  `recv_data_stream_closed` は stream id しか受け取らないため、破棄対象とした stream id から request ID を
  逆引きできる対応表 (`DataStreamId` → request ID) を許容集合と同時に保持する。`forget_fetch` が受信 stream を
  破棄対象の保持集合へ移す場合も、同じ対応表へ登録する。FIN / RESET_STREAM の観測時は、`Discarded` variant
  が持つ request ID、開いたままの `IncomingDataStream::Fetch` の request ID、保持集合の stream id の逆引きの
  いずれかで許容集合と対応表の両方から除去する。FETCH_HEADER を受信しないまま FIN / RESET_STREAM した
  stream は request ID を特定できないため、許容集合は保持期間の経過まで残る (2 本目を拒否できない既知の
  制約)。また GOAWAY の request stream reset 由来の許容 (`goaway.request_stream_sent`) は FIN では解除
  されない既存挙動であり、REQUEST_ERROR 起因の許容集合とは独立に残る。
- `Session::recv_fetch_entry` は破棄対象 stream への entry を no-op で受理する (`IncomingDataStream::Discarded`
  と保持集合の stream id の両方)。Object / End of Range のどちらも同じ API で受容する。
  `Session::recv_data_stream_type` は保持集合の stream id を no-op で吸収する既存挙動を維持する (request ID は
  FETCH_HEADER まで判明しないため、許容判定は header 受信時に行う)。`Session::recv_data_stream_closed` は
  終端観測時に上記の除去を行う。
- 既に open な `IncomingDataStream::Fetch` がある状態で REQUEST_ERROR を受けても、その stream は
  `IncomingDataStream::Fetch` のまま維持し、破棄対象への変換は行わない (`recv_fetch_entry` は Fetch variant を
  state によらず受理するため、`forget_fetch` 時の保持判定だけを確認する)。
- `tests/test_session/fetch/after_fin.rs` に、`recv_data_stream_type` → `recv_fetch_header` → `recv_fetch_entry`
  (Object と End of Range) → `recv_data_stream_closed` (FIN) の順で届いてもセッションが `Established` のまま
  `Ok` になること、`forget_fetch` 後でも同じであること、REQUEST_ERROR を受けていない終端済み fetch への遅延
  header は従来どおり `PROTOCOL_VIOLATION` になることを追加する。保持期間の経過後は許容が解除されること、
  `Established` + REQUEST_UPDATE 失敗応答の経路、RESET_STREAM 終端、2 本目 stream の拒否も固定する。

## 完了条件

- `Pending` の fetch が peer の REQUEST_ERROR で終端した後、その request ID の `recv_data_stream_type` /
  `recv_fetch_header` / `recv_fetch_entry` (Object と End of Range) / `recv_data_stream_closed` (FIN) がすべて
  `Ok` を返し、セッションが `Established` のままであること (`CloseSession` が発行されないこと)。
- `Established` の fetch が REQUEST_UPDATE 失敗応答の REQUEST_ERROR で終端した後も、同じ data stream の受容が
  成立すること。
- `forget_fetch` 後でも同じ順序の data stream がセッションを閉じないこと。
- 許容集合に記録されていない FIN 終端済み fetch への `recv_fetch_header` は従来どおり `PROTOCOL_VIOLATION` で
  閉じること (GOAWAY 由来の許容は既存どおり成立し、既存テストが退行しないこと)。
- 1 本目の data stream が開いている間に同じ request ID の 2 本目の `recv_fetch_header` が届いた場合は、
  `PROTOCOL_VIOLATION` で閉じること (`has_other_fetch_stream` が `Discarded` も数えることの固定)。
- FETCH_HEADER を受理済みの対象 data stream の FIN を観測した後、同じ request ID の 2 本目の
  `recv_fetch_header` は許容されず `PROTOCOL_VIOLATION` で閉じること (request ID は header まで判明しない
  ため、`recv_data_stream_type` の時点では判定しない)。`forget_fetch` を挟んだ経路でも同じであること。
  この判定は REQUEST_ERROR 起因の許容集合の解除を根拠とし、GOAWAY の request stream reset 由来の許容が
  併存する場合は既存挙動どおり 2 本目が受理されうる。FETCH_HEADER を受信しないまま FIN した stream は
  request ID を特定できないため、保持期間の経過まで許容が残る。
- `Established` の fetch で REQUEST_UPDATE 失敗応答の REQUEST_ERROR を受けた後、および data stream を
  RESET_STREAM で終端した後も、許容集合の解除が正しく行われること。
- `peer_alias_retention_ms` の保持期間経過後は許容記録が期限切れになり、同じ FETCH_HEADER が
  `PROTOCOL_VIOLATION` で閉じること。
- 既存の FIN → REQUEST_ERROR の順序と、GOAWAY 遅延 header の既存テストが退行しないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
