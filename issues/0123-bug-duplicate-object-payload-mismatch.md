# 重複 Object の Payload / immutable properties の差異を検出できるようにする

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-duplicate-object-payload-mismatch
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §7.1 (Caching Relays) は、重複して受信した Object の Payload が異なる場合を Malformed Track とする MUST を定める。

> An endpoint that receives a duplicate Object with a different Forwarding Preference, Subgroup ID, Priority or Payload MUST treat the track as Malformed.

§12.1 (Malformed Tracks) の条件 6 も同じ条件を immutable properties に広げている。

> The same Object is received more than once with different Payload or other immutable properties.

現状はこの検出手段が無く、内容の異なる同一 Object をそのままアプリへ渡す。

## 現状

- `src/object_properties.rs` の `ObjectFieldTracker::observe_object_fields` は group_id / object_id / is_subgroup / subgroup_id / publisher_priority しか受け取らない。`ObjectFieldRecord` も is_subgroup / subgroup_id / publisher_priority のみを保持する。Payload と IMMUTABLE_PROPERTIES の raw バイト列は比較できない
- `src/object_properties.rs` の `ObjectFieldTracker` の doc コメントに、Payload と IMMUTABLE_PROPERTIES を保持しないことが意図的な未実装として書かれている。`src/session/data.rs` の `accept_subgroup_object` にある `observe_object` のエラー処理のコメントにも「Session では検出しない (app / relay 層の責務)」とある
- Session は payload のバイト列を持たない。`src/stream/decoder.rs` の `DecodedSubgroupObject` は object_id / payload_length / status / properties_bytes のみで、payload は `SubgroupStreamDecoder::try_read_payload` でアプリが読む。`ObjectDatagram` も payload を保持しない
- 一方 IMMUTABLE_PROPERTIES の raw バイト列は `ObjectProperties::immutable_properties` で取得でき、Session の呼び出し元には `properties_bytes` がある。つまり immutables は Session でも比較できる
- `tests/test_object_properties.rs` の `object_field_mismatch_converts_into_boxed_error` は Forwarding Preference の差だけを固定しており、Payload / immutables のテストは無い

## 設計方針

- Payload と immutables の生バイト列を Session が保持しない方針は維持しつつ、呼び出し側が比較に参加できる API を追加する。判断を記録して終わらせず、実装可能な最小の手段を実装する
- `ObjectFieldTracker` の記録に (a) immutable properties のダイジェスト、(b) payload のダイジェストを追加し、固定長だけを保持する。保持量は Object 数 × 固定長となり、入力サイズに比例しない
- 入口は次の 2 つを用意し、どちらを主とするかを実装時に確定する。(1) 生バイト列 (`Option<&[u8]>`) を渡す入口: payload と immutables の生バイト列を持つ層が使う。(2) 呼び出し側が算出した固定長ダイジェスト (`Option<&[u8]>`) を渡す入口: 独自のハッシュを使う層が使う。確定しなかった側は doc で代替手段を示す
- Session 経路: immutables は `properties_bytes` から `ObjectProperties::immutable_properties` で取得して渡し、immutables の差を Malformed Track として検出する。payload は Session がバイト列を持たないため、`DecodedSubgroupObject` の payload_length と status を渡し、「長さまたは status が異なれば確実に異なる」場合のみ検出する。同じ長さで内容が異なるケースは payload 生バイト列を持つ層がダイジェストを渡して検出する
- ダイジェストの算出方法 (crate 内の固定長ハッシュ関数か、呼び出し側供給か) を確定し、衝突した場合は「不一致と判定されない」側に倒れる (見逃しになる) ことを doc に明記する。Malformed Track の誤検出で正常な track を落とすより見逃し側に倒す
- 違反時は既存と同じ `ObjectFieldMismatch` を返し、Session の malformed track 経路 (`terminate_malformed_track`) に乗せる。新しいエラー型は増やさない
- `ObjectFieldTracker` と `src/session/data.rs` の「検出しない」というコメントを実装に合わせて書き換える。何を Session で検出でき、何を payload 生バイト列を持つ層に委ねるのかをコメントに残す

## 完了条件

- 同じ (Group, Object) を異なる immutable properties で観測したときに Malformed Track として検出できることを固定するテストが追加されていること (tracker 単体と Session 経由の両方)
- 同じ (Group, Object) を異なる Payload で観測したときに Malformed Track として検出できることを固定するテストが追加されていること。Session 経由では payload_length または status の差で検出できることを固定し、同じ長さで内容が異なるケースはダイジェストを渡す経路の tracker 単体テストで固定する
- immutables と payload が一致する重複 Object は引き続き受理されることを固定するテストが追加されていること (非退行)
- Forwarding Preference / Subgroup ID / Priority の既存検出が非退行であること
- 保持する記録が固定長であり入力サイズに比例しないことが doc に明記されていること
