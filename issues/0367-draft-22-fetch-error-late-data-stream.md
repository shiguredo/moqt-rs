# FETCH_ERROR 受信後に遅延する in-flight FETCH data stream を破棄として受理する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.2 (Fetch) は publisher が FETCH_OK より前に Object を送ることを許し、FETCH_OK 送信前に FETCH_ERROR を送る場合も data stream
の reset と併用できると定める。§3.2.4 (Fetch State Management) は "Since a relay can start delivering FETCH Objects from cache before
determining the result of the request, some Objects could be received even if the FETCH results in error." と明記する。つまり FETCH_ERROR
を受信した後に in-flight の FETCH data stream (FETCH_HEADER / Object) が届くのは正当な競合である。現状はこれを不正として `PROTOCOL_VIOLATION` でセッションを閉じるため、準拠した
publisher との間でセッションが落ちる。

## 現状

- `src/session/fetch.rs` の `handle_err_for_fetch` は REQUEST_ERROR 受信時に fetch を `Terminated` にして `response_received` を立てるだけで、遅延
  data stream を許容する記録を残さない。
- `src/session/data.rs` の `recv_fetch_header` は fetch が `Terminated` のとき `Session::tolerates_late_data_stream` を確認し、許容されない場合は
  `FETCH_HEADER received for terminated fetch` で `fail` する。
- `src/session/core.rs` の `Session::tolerates_late_data_stream` は `rejected_request_ids` (自側が request を拒否・終端した場合) と request
  stream GOAWAY の ID だけを対象にしており、peer の REQUEST_ERROR で終端した fetch は含まれない。
- `Session::forget_fetch` で fetch を回収した後に data stream が届く経路も、未知の request ID として `PROTOCOL_VIOLATION` になる。
- `tests/test_session/fetch/after_fin.rs` は FIN → FETCH_ERROR の順序を固定しているが、FETCH_ERROR → data stream の順序は未検証である。

## 設計方針

- fetch が peer の REQUEST_ERROR で `Terminated` になったとき、その request ID を「遅延 data stream を許容する fetch」として記録する。記録は `forget_fetch`
  後も維持し、対象 data stream の FIN / RESET_STREAM を観測した時点で除去する。data stream が一度も届かない場合の保持量は session 内の fetch request 数に比例する有界の集合とする。
- `recv_fetch_header` は許容対象の request ID に対して、fill fetch stream と同じく `register_discarded_stream` で破棄対象として登録し、セッションを閉じない。以降の Object
  と payload は既存の破棄対象 stream の扱いで読み捨てる。
- すでに open な FETCH data stream がある状態で REQUEST_ERROR を受けた場合も、その stream への Object 到着が `PROTOCOL_VIOLATION`
  にならないことを確認し、必要なら同じ破棄扱いに寄せる。
- `tests/test_session/fetch/` に、REQUEST_ERROR → FETCH_HEADER → Object → FIN の順で届いてもセッションが閉じないこと、`forget_fetch`
  後でも同じであること、REQUEST_ERROR を受けていない終端済み fetch への遅延 header は従来どおり `PROTOCOL_VIOLATION` になることを追加する。

## 完了条件

- Pending の fetch が peer の REQUEST_ERROR で終端した後、その request ID の FETCH_HEADER / Object / FIN が届いてもセッションが閉じず、破棄されること。
- `forget_fetch` 後でも同じ順序の data stream がセッションを閉じないこと。
- 許容記録のない request ID の FETCH_HEADER は従来どおり `PROTOCOL_VIOLATION` で閉じること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
