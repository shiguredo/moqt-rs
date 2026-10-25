# publisher 側 FETCH / fill fetch の応答 Object を要求フィルタで検証する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.3.1 (Location Filters) は "A publisher MUST NOT send objects from outside the requested range." と
MUST を課し、§3.3.3 (Combining Filters) は "The publisher MUST forward only objects that pass all filters." と MUST を課す。§3.4 (Fill
Semantics) は fill fetch stream が subscription の Range Filter を継承し、FILL_PARAMETERS 内のパラメータが外側を上書きすると定める。subscription の送信経路は
`object_passes_filters` でこれを強制しているが、FETCH / fill fetch の応答経路は Object の Location も Properties も検証していないため、ライブラリ利用者が MUST 違反の Object
を送れてしまう。

## 現状

- `src/session/data.rs` の `Session::send_fetch_object` は `outgoing_fetch` / `outgoing_fill` に登録済みの stream かどうかだけを検証し、Location と
  Object Properties を受け取らない。Object のペイロードと Location はアプリが直接 wire に書くため、Session は何も検査できない。
- `src/session/fetch.rs` の publisher 役 `Fetch` は `fetch_start` を保持しない (コメントに「publisher 側は FETCH_OK の End 検証を行わないため要求 Start
  を保持しない」とある)。要求された LOCATION_FILTER と Range Filters も保持しない。
- `src/session/data.rs` の `OutgoingFetchStream` (fill fetch stream) は `request_id` だけを保持し、fill range と継承する Range Filters を保持しない。
- `src/session/subscription/validation.rs` の `object_passes_filters` は subscription の Location Filter / Range Filters と Object の
  Location / Properties を評価する。subscription の `send_subgroup_object` / `send_object_datagram` はこの関数で送信前に拒否する。
- `examples/tokio-moq/src/moqt_client.rs` と `tests/test_session/fetch/` が `send_fetch_object` を呼び出している。

## 設計方針

- publisher 役 `Fetch` に、要求された LOCATION_FILTER から解決した start / end と Range Filters を保持する。FETCH 受信時に `extract_range_filters` で抽出する。
- fill fetch stream を開く経路で、FILL_PARAMETERS の上書きを反映した effective fill range と継承する Range Filters を計算して保存する。計算は既存の
  `src/session/subscription/fill.rs` の解決処理を再利用する。
- `send_fetch_object` を Location と Object Properties の生バイト列を受け取る形に変更し、要求範囲外またはフィルタ不通過なら subscription の送信経路と同じ
  `SendRequestError::LocalFilterMismatch` を返して拒否する。戻り値の型も `Result<(), SendRequestError>` に変更する。
- `examples/` と `tests/` の呼び出し側を新しいシグネチャへ追従させる。
- Session は「アプリが送ろうとした Object を拒否する」ところまでを担い、wire encode は従来どおりアプリが行う。
- `docs/moqt.md` の該当記述を実装に合わせて更新する。

## 完了条件

- 要求 Location Filter の範囲外の Location で `send_fetch_object` を呼ぶと `SendRequestError::LocalFilterMismatch` が返り、セッションは閉じないこと。
- 要求 Range Filters を通らない Object Properties で `send_fetch_object` を呼ぶと同じく拒否されること。
- fill fetch stream では FILL_PARAMETERS の上書きを反映した range で判定されること。
- FETCH_OK より前の Object 送信 (MAY) や、FETCH の End Location を超えないことの検証など、本 issue の範囲外の順序契約を変えないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
