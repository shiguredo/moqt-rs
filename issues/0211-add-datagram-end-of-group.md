# Datagram で END_OF_GROUP を送れるようにする

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-datagram-end-of-group
- Polished: {YYYY-MM-DD}

## 目的

Object Datagram で Group の終端 (END_OF_GROUP) を通知できるようにする。

END_OF_GROUP bit は「同じ Group ID で、この datagram の Object ID より大きい Object ID の Object は
存在しない」ことを宣言する (draft-ietf-moq-transport-22 §11.2.1 (Object Datagram))。datagram で送れると、
subgroup ストリームを開かずに Group の区切りを伝えられる。E2E テストから datagram 経路だけで
Group の終端を再現できるようになる。

moqt-py の `Publication.send_datagram` から END_OF_GROUP を指定できるようにするための前提でもある。
moqt-py はフィルタ評価や重複 Object 検証を状態機械 (moqt-rs) に委ねる設計のため、
moqt-rs の送信 API に引数が無い状態で moqt-py 側だけが bit を立てると状態機械の判断と食い違う。

## 現状

- `src/session/data.rs` の `Session::send_object_datagram` は
  `(request_id, group_id, object_id, properties_data, status)` を受け取るが、
  END_OF_GROUP bit を指定する引数が無い。呼び出し元は END_OF_GROUP を宣言できない。
- エンコーダ (`src/stream/datagram.rs` の `ObjectDatagram::end_of_group` と `ObjectDatagram::encode`) と
  受信側 (`Session::recv_object_datagram` の `record_group_end_after`) は END_OF_GROUP に対応済みである。
  欠けているのは送信 API だけである。
- `ObjectDatagram::encode` と `Session::recv_object_datagram` は STATUS (0x20) と END_OF_GROUP (0x02) の
  同時指定を無効な Type 値として拒否する (draft-ietf-moq-transport-22 §11.2.1 (Object Datagram)) が、
  `Session::send_object_datagram` にはこの組み合わせを拒否する経路が無い (引数が無いため)。
- moqt-py の native バインディング、`moqt.moq._runtime.Runtime.send_object_datagram`、
  `moqt.moq.publisher.Publication.send_datagram` も END_OF_GROUP を運べない。

## 設計方針

- `Session::send_object_datagram` の末尾に `end_of_group: bool` を追加する。検証・フィルタ評価・
  最大位置更新の順序は現行を維持する。
- STATUS と END_OF_GROUP の同時指定は、エンコーダ・受信側と同じく `SESSION_PROTOCOL_VIOLATION` で
  拒否する。拒否時は状態を汚染しないよう、フィルタ評価と最大位置更新より前に判定する。
- END_OF_GROUP は Object の Location を変えないため、フィルタ評価 (`object_passes_filters`) の入力は
  変更しない。受信側では既存の `record_group_end_after` が Group の終端を
  「存在しない最小の Object ID」として記録し、`object_after_track_end` が宣言位置より大きい Object を
  Malformed Track として扱う。この一連の挙動をテストで固定する。

## 完了条件

- `Session::send_object_datagram` で END_OF_GROUP を指定できること。
- STATUS と END_OF_GROUP の同時指定が拒否されること。
- publisher が送った END_OF_GROUP datagram を subscriber が受信すると、その Group の終端が記録され、
  宣言位置より大きい Object が Malformed Track として拒否されること。
