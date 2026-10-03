# QUIC / WebTransport のプロトコル識別子を moqt-22 に更新する

- Created: 2026-10-03
- Completed: 2026-10-03
- Branch: feature/change-moqt-protocol-22
- Polished: {YYYY-MM-DD}

## 目的

relay が draft-22 の識別子 (`moqt-22`) を要求するようになり、E2E の PUBLISH が `NO_APPLICATION_PROTOCOL` で失敗している。クライアント (`examples/tokio-moq`) のプロトコル識別子は `moqt-21` のままで、draft-22 の ALPN 規則 (`moqt-` + draft 番号) と一致していない。識別子を `moqt-22` に更新する。

## 現状

- `examples/tokio-moq/src/lib.rs` の `MOQT_PROTOCOL` は `"moqt-21"`。QUIC の ALPN、WebTransport の `WT-Available-Protocols`、`WT-Protocol` の検証がすべてこの定数を使う (`examples/tokio-moq/src/quic.rs` / `webtransport_h2.rs` / `webtransport_h3.rs`)
- `examples/tokio-moq/src/webtransport_h2.rs` のエラーメッセージとテスト (`verify_wt_protocol` とその呼び出し) に `moqt-21` のリテラルがある
- `examples/README.md` の伝送路の表と URL スキームの節に `moqt-21` の記載がある
- E2E (実 relay への PUBLISH) は失敗している

  ```text
  QUIC: connection failed: The connection was closed on the transport level
  with error NO_APPLICATION_PROTOCOL by the remote endpoint
  ```

  最後に成功した E2E の実行 (0196) 以降、リポジトリ側のコード変更は無く、relay が `moqt-22` を要求するようになったとみられる
- draft-22 の一次資料は「IETF draft の ALPN は `moqt-` に draft 番号を付けたもの」と定めており (`refs/moq/draft-ietf-moq-transport-22.txt` §6.2)、draft-22 の識別子は `moqt-22` である
- issue 0195 / 0196 の設計方針にある「`moqt-21` は draft-22 でも正しいため変更しない」は上記と矛盾する。本 issue がその前提を上書きする

## 設計方針

- `MOQT_PROTOCOL` を `"moqt-22"` に変更する。ALPN / `WT-Available-Protocols` / `WT-Protocol` の検証は定数経由で追従する
- `webtransport_h2.rs` のエラーメッセージとテストのリテラル、`lib.rs` の doc コメントを `moqt-22` に更新する
- `examples/README.md` の `moqt-21` の記載 (伝送路の表、URL スキームの節) を `moqt-22` に更新する
- `moqt-21` と `moqt-22` を併記して互換を取る移行措置は取らない。relay は 1 つの識別子で運用するため
- issue 0195 / 0196 の本文は変更しない (経緯は本 issue に残す)

## 完了条件

- `examples/tokio-moq` が提示する ALPN / `WT-Available-Protocols` が `moqt-22` になること (単体テストで固定する)
- `MOQT_PROTOCOL` を参照する経路と利用者向けドキュメント (`examples/README.md`) から `moqt-21` が消えること (別プロトコルとして拒否するテストのリテラルと過去の issue は対象外)
- E2E (実 relay への PUBLISH) が成功すること
- `cargo test --workspace` と `cargo clippy --workspace --all-targets -- -D warnings` が通ること

## 解決方法

- `examples/tokio-moq/src/lib.rs` の `MOQT_PROTOCOL` を `moqt-22` に変更した。ALPN / `WT-Available-Protocols` / `WT-Protocol` の検証は定数経由で追従する
- `examples/tokio-moq/src/webtransport_h2.rs` のエラーメッセージとテスト (`verify_wt_protocol_accepts_moqt_22` など)、`examples/tokio-moq/src/quic.rs` の節参照、`examples/README.md` の伝送路の表と URL スキームの節を更新した
- 別プロトコルとして拒否するテストのリテラル (`moqt-21`) は残している
- feature ブランチの CI と E2E (実 relay への PUBLISH) が成功することを確認した。relay が `moqt-22` を要求していることが確認できた
