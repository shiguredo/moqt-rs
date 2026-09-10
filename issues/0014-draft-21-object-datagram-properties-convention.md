# send_object_datagram と ObjectDatagram の properties_data 規約を統一する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-datagram-properties-convention

## 目的

Object Datagram の Object Properties がワイヤ上で 1 回だけ Length prefix を持つようにする。現状は送信 API が「Length 込み」と「Length 抜き」を同時に要求しており、example が壊れたワイヤを生成する。

## 現状

`src/session/data.rs` の `send_object_datagram` は引数 `properties_data` を `ObjectFilterInput.properties_bytes` に渡す。`properties_bytes` は `Properties Length varint + データ` を要求する (`src/session/subscription/validation.rs` の `object_passes_filters`)。

同じ値を `ObjectDatagram::encode` (`src/stream/datagram.rs`) にも渡すが、こちらは `properties_data` を Length 抜きとして扱い、自分で `varint::encode(data.len())` を前置する。`decode` も Length を除去して返す。受信 API の `recv_object_datagram` は Length 抜きに Length を付け直しており、送受信で非対称。

`LocProperties::encode()` は Length 込みを返す (`src/loc.rs`)。`examples/moqt-publisher/src/datagram_writer.rs` は `properties.encode()` を `ObjectDatagram.properties_data` に入れるため、ワイヤは `Properties Length | Properties Length | Key-Value-Pairs...` になり、受信側で properties が誤パースされる。

根拠 (draft-ietf-moq-transport-21 §11.1.3 / Figure 24): Object Properties は Length を 1 回だけ持つ。

## 設計方針

どちらか一方の規約へ統一する。

- `send_object_datagram` を Length 抜きに統一し、内部でフィルタ評価用に Length を前置する
- または `ObjectDatagram` を Length 込みに変更し、`decode` も Length 込みで返す

example (`datagram_writer.rs`) も統一後の規約に合わせて修正する。

## 完了条件

- Object Datagram の Object Properties がワイヤ上で Length を 1 回だけ持つこと
- `send_object_datagram` / `recv_object_datagram` / `ObjectDatagram::encode` / `decode` の規約が一致すること
- example が生成する datagram が受信側で正しくパースできること
- ラウンドトリップテストが追加されていること
