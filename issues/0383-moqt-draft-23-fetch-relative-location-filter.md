# 相対 Location Filter を持つ FETCH を INVALID_RANGE で拒否する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-fetch-relative-location-filter
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.2、§9.19.9](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/fetch.rs` の `handle_peer_fetch` は `location_filter_typed` の結果を `effective_start_location` / `effective_end_location` で解決するため、`RelativeGroup` / `NextObject` を絶対 FETCH と同じように処理する。
`send_fetch` も相対形式自体を禁止していない。

## 設計方針

単独 FETCH の相対 Location Filter を、フィルタの形式に基づいて早期に判別する。
受信時はセッションを維持して REQUEST_ERROR (INVALID_RANGE) を返し、Fetch state やデータ配送を開始しない。
送信 API は不正な単独 FETCH を送らない。
FILL_PARAMETERS の相対フィルタは正当なので、この禁止を流用しない。

## 完了条件

- 相対開始と Next Object の単独 FETCH が INVALID_RANGE で拒否されること。
- 絶対範囲・NoFilter・省略は従来どおり判定されること。
- fill の相対フィルタが引き続き利用でき、拒否時に request / data state が残らないこと。
