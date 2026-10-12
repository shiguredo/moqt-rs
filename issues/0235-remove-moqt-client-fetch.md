# example の MoqtClient::fetch を削除する

- Created: 2026-10-12
- Completed: 2026-10-12
- Branch: feature/remove-moqt-client-fetch
- Polished: {YYYY-MM-DD}

## 目的

`examples/tokio-moq` の `MoqtClient::fetch` は example からもテストからも呼ばれておらず、
到達不能な公開 API になっている。0224 でカタログ取得を FILL_PARAMETERS 付き SUBSCRIBE に
寄せてから利用経路が無く、0223 のように「誰も実行しない経路」のレビューと検証のコストだけが
増えるため削除する。なお 0223 はこの経路の認可トークン付与を修正した issue であり、本削除で
その到達点の一部 (example が FETCH を送る前提) が無効になる。0223 の解決方法が記録として
残ることは問題ないが、README と doc コメントの列挙は本 issue で実態に合わせる。

## 現状

- `MoqtClient::fetch` を呼ぶ箇所はリポジトリに無い。`examples/moq-sub` は catalog を
  FILL_PARAMETERS 付き SUBSCRIBE で取得し、FETCH を発行しない (0224 の解決方法に
  「`MoqtClient::fetch` を使う経路は example から無くなった」と記録済み)。open issue にも
  example から FETCH を発行する計画は無い。
- `MoqtClient::fetch` は `FetchResult` / `build_fetch_result` / `wait_fetch_resolved` だけから
  成る。`FetchResult` が保持するのは `request_id` のみで、FETCH_OK の `end_location` /
  `end_of_track` は受け取らない。
- example は FETCH を「受ける」側は実装している。`examples/moq-pub` は `send_fetch_ok` /
  `send_fetch_response` で peer の FETCH に応答し、fill fetch stream は `send_fill_fetch_header` /
  `recv_fetch_header` / `recv_fetch_entry` などで扱う。削除対象は FETCH を「発行する」経路だけである。
- `MoqtClient` は実トランスポートでのみ構築でき、モック / スタブの利用は禁止されているため、
  `fetch` の送信経路は example のテストでは固定できない (0223 の解決方法に制約として記載済み)。

## 設計方針

- `MoqtClient::fetch` / `FetchResult` / `build_fetch_result` / `wait_fetch_resolved` を削除する。
- FETCH を受けて応答する経路と fill fetch stream の経路は残す (`DataPlaneHandle` の
  `send_fetch_*` / `recv_fetch_*`、`send_fetch_ok` / `send_fetch_response` /
  `send_fill_fetch_response` / `reject_fill_fetch`)。
- 削除で未使用になる import があれば整理する (`FetchState` は FETCH 応答データストリームの
  STOP_SENDING 処理で使い続けるため残す)。
- README / docs / doc コメントから、example が FETCH を発行する・FETCH にもトークンを付与する
  と読める記述を除く (0223 が付与先として FETCH を列挙した記述を含む)。

## 完了条件

- `MoqtClient::fetch` とその関連シンボルが消え、参照が残っていないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MoqtClient::fetch` / `FetchResult` / `build_fetch_result` / `wait_fetch_resolved` を削除した。
  example に FETCH を発行する経路は無く (0224 でカタログ取得を FILL_PARAMETERS 付き SUBSCRIBE に
  寄せた時点で呼び出し元が消えている)、open issue にも追加する計画が無い。
- FETCH を受けて応答する経路 (`send_fetch_ok` / `send_fetch_response` /
  `send_fill_fetch_response` / `reject_fill_fetch`) と fill fetch stream の `DataPlaneHandle` API は
  そのまま残した。`FetchState` は FETCH 応答データストリームの STOP_SENDING 処理で使い続ける。
- 削除に伴い、example が FETCH を発行する・FETCH にもトークンを付与すると読める記述を実態に
  合わせた (`MoqtClient` のモジュール doc / `auth_tokens` / `with_auth_parameters` /
  `establish_*` / `send_request_update` / `drain_events` / テスト doc、`README.md`、
  `examples/README.md`)。トークンを付与する送信経路の列挙は PUBLISH / SUBSCRIBE /
  REQUEST_UPDATE の 3 つになった。
- 0223 が `MoqtClient::fetch` の認可トークン付与を修正していたため、本削除でその到達点の一部は
  無効になる。0223 の解決方法は記録としてそのまま残し、README と doc コメントの列挙だけを
  本 issue で直した。
- 本作業で起票した関連 issue (実装は別途): 0236 (request stream の先頭メッセージの扱い)、
  0237 (§8.9 の alias 解決後の一意性)、0238 (KVP の宣言長超過の終了コード)、
  0239 (手組み SETUP の一意性検証)、0240 (初回 SETUP 交換の失敗時の終了コード)、
  0241 (未対応 request の REGISTER 適用)、0242 (in-process の MOQT peer 試験基盤)。
