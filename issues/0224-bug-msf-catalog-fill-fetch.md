# カタログ取得を FILL_PARAMETERS 付き SUBSCRIBE に寄せる

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-catalog-fill-fetch
- Polished: 2026-10-10

## 目的

draft-ietf-moq-msf-01 §5 (Catalog) は「Subscribers accessing the catalog MUST use SUBSCRIBE with a
Joining FETCH (offset = 0) in order to obtain the latest complete catalog along with all
subsequent catalog objects, including delta updates, that follow.」と MUST を定める。
draft-ietf-moq-transport-22 は Appendix A.3 で Joining FETCH を削除し、fill fetch stream (§3.4)
と FILL_PARAMETERS (§9.20.15) に置き換えた。同 draft §3.5 (Joining an Ongoing Track) は
「To join a Track at the current Group, the subscriber sends a SUBSCRIBE with a Location Filter
that starts at the Next Object and a FILL_PARAMETERS parameter whose Location filter has
StartGroup=1, which fills the current Group from its start.」と、Joining FETCH (offset = 0) に
相当する手段を示している。購読と fill が 1 要求になり、最新の完全カタログと後続 delta を
隙間なく取得できる。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `run` は `MoqtClient::subscribe_track_with_filter`
  (`LocationFilter::NextObject`) と、SUBSCRIBE_OK の `LARGEST_OBJECT` から作る別の
  `MoqtClient::fetch` (`catalog_fetch_filter`) の 2 要求でカタログを取得する。
- 購読側で先に届いた delta は `examples/moq-sub/src/catalog.rs` の `CatalogState::apply` が
  「delta without an independent catalog」として捨てる。
- FETCH は応答時点の Largest Object までしか返さない (§3.2 (Fetch)) ため、その後に publish
  された delta は購読側でしか届かない。購読で届いた delta が FETCH 応答より先に処理されると、
  基底が無いまま捨てられ、再取得もされず変更が失われる。
- `receive_catalog` の doc は「FETCH が失敗しても購読は生かしたままにして、届いた Object だけで
  解決を試みる」と書くが、`client.fetch(...).await?` は FETCH_ERROR で example を終了させる。
  この終了の扱いは本 issue では扱わない。
- `src/session/subscription/fill.rs` には FILL_PARAMETERS の検証と fill fetch stream の処理が
  既にある。一方 `MoqtClient::subscribe_track_with_filter` は LocationFilter しか受け取らず、
  example から FILL_PARAMETERS を付ける API が無い。
- publisher 側にも fill の配信経路が無い。Session は FILL_PARAMETERS 付き SUBSCRIBE の処理で
  `SessionEvent::OpenFillFetchStream` を発火するが、fill fetch stream を開いて FETCH_HEADER を
  書くのはアプリの役割であり (`src/session/data.rs` の `send_fill_fetch_header`)、
  `examples/tokio-moq/src/moqt_client.rs` は `OpenFillFetchStream` を無視する
  (draft-ietf-moq-transport-22 §3.4.1 (Opening and Closing Fill Fetch Streams))。このため
  `examples/moq-pub` は FILL_PARAMETERS 付き SUBSCRIBE に fill fetch stream で応答せず、
  subscriber 側だけの変更では本 issue の完了条件を満たせない。

## 設計方針

- カタログの購読に FILL_PARAMETERS (type 0x23) を付ける。内側の LOCATION_FILTER は現在 Group の
  先頭から埋める指定 (`LocationFilter::RelativeGroup { start_group: 1 }` 相当) にする。
  §3.4 は fill range の解決と FIN / RESET の規則を定めているため、その規則に従う。
- `MoqtClient` にパラメータを渡せる購読 API を追加し、LARGEST_OBJECT の観測と別 FETCH は fill で
  代替できるなら削除する。
- 受信側の `receive_catalog` は fill fetch stream の受理経路へ切り替え、購読で届く Object と
  fill で届く Object を同じ `CatalogState` へ到着順に適用する。
- publisher 側は `OpenFillFetchStream` を受けて uni stream を開き、fill の FETCH_HEADER に
  SUBSCRIBE の Request ID を載せて (§3.4) fill range のカタログ Object を配信する経路を
  `examples/moq-pub` に追加する。moq-pub が publish するカタログは Group の先頭 Object だけ
  なので、fill range の判定は moq-pub の `catalog::catalog_fetch_response` を流用できる。
- Group ID を 0 と仮定しない扱いと、独立カタログを Group の先頭 Object とする前提は維持する。

## 完了条件

- カタログの取得が FILL_PARAMETERS 付き SUBSCRIBE 1 つで完結し (publisher が fill fetch stream
  で応答することを含む)、独立カタログより先に delta を処理する経路が無いこと。
- fill fetch stream で届く独立カタログと delta を `CatalogState` が適用できることのテスト。
- Group ID を 0 と仮定しないこと (Largest Object 由来の値を使うこと)。
- doc が新しい取得経路に更新されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
