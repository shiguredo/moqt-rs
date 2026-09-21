# 重複 Object の immutable properties の差異を検出できるようにする

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-duplicate-object-payload-mismatch
- Polished: 2026-09-21

## 目的

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の条件 6 は、同じ Object を重複して受信したときに Payload または immutable properties が異なれば Malformed Track と定める。

> The same Object is received more than once with different Payload or other immutable properties.

§7.1 (Caching Relays) にも重なる条件があるが、対象が一致しない。§7.1 は Forwarding Preference / Subgroup ID / Priority / Payload を、§12.1 条件 6 は Payload / immutable properties を対象にする。本ライブラリは relay に対応しない (CODEBASE.md) ため、endpoint に適用される §12.1 条件 6 を根拠にする。

現状は immutable properties の差異を検出する手段が無く、内容の異なる同一 Object をそのままアプリへ渡す。

## 現状

- `src/object_properties.rs` の `ObjectFieldTracker::observe_object_fields` は group_id / object_id / is_subgroup / subgroup_id / publisher_priority しか受け取らない。`ObjectFieldRecord` も is_subgroup / subgroup_id / publisher_priority のみを保持するため、immutable properties と payload は比較できない
- `src/object_properties.rs` の `ObjectFieldTracker` の doc コメントに、Payload と IMMUTABLE_PROPERTIES を保持しないことが意図的な未実装として書かれている。理由として Sans I/O + no_std 制約下で payload の保持を許容できないことが挙げられている
- `src/session/data.rs` の `accept_subgroup_object` にある `observe_object` のエラー処理のコメントにも「Session では検出しない (app / relay 層の責務)」とある
- Session は payload のバイト列を持たない。`src/stream/decoder.rs` の `DecodedSubgroupObject` は object_id / payload_length / status / properties_bytes のみで、payload は `SubgroupStreamDecoder::try_read_payload` でアプリが読む
- `Session::recv_subgroup_object` は `&DecodedSubgroupObject` を、`Session::recv_object_datagram` は `&ObjectDatagram` を受け取る。payload_length は subgroup 経路でのみ参照でき、status は両経路で参照できる。どちらの経路でも参照できないのは payload のバイト列そのものである
- 一方 IMMUTABLE_PROPERTIES の生バイト列は `ObjectProperties::immutable_properties` で取得でき、Session の呼び出し元には `properties_bytes` がある。immutables は Session でも比較できる
- `tests/test_object_properties.rs` の `object_field_mismatch_converts_into_boxed_error` は Forwarding Preference の差だけを固定しており、immutables / payload のテストは無い
- `ObjectFieldTracker::observe_object_fields` の呼び出し元は `tests/test_object_properties.rs` / `pbt/tests/prop_object_tracker.rs` / `fuzz/fuzz_targets/fuzz_object_trackers.rs` / `src/session/data.rs` である

## 設計方針

- 検出できる範囲を次のように分ける
  - Session は immutables の差異を検出する。Session は `properties_bytes` から `ObjectProperties::immutable_properties` で内側の生バイト列を取得でき、バイト列を保持しない方針とも両立する
  - Session は payload の比較キーとして payload_length と status を符号化した固定長キーを渡す。長さまたは status が異なる重複は決定的に検出できる。同じ長さで内容だけが異なる場合は Session では検出できない
  - payload のバイト列を持つ層 (アプリ) は、内容の比較キー (ダイジェスト等) を渡して同じ長さの差異も検出できる。現行 doc の「Session では検出しない (app / relay 層の責務)」という判断は payload の内容について維持し、immutables と payload の長さ・status についてだけ覆す
- `ObjectFieldTracker` に `observe_object_fields_with_content` を追加する。引数は `group_id` / `object_id` / `is_subgroup` / `subgroup_id` / `publisher_priority` に `immutable_properties: Option<&[u8]>` と `payload_key: Option<&[u8]>` を加えた 7 個とする
- 既存の `observe_object_fields` はシグネチャを変えず、`immutable_properties: None` / `payload_key: None` で `observe_object_fields_with_content` に委譲する
- これにより公開 API の破壊的変更を避け、`tests/test_object_properties.rs` / `pbt/tests/prop_object_tracker.rs` / `fuzz/fuzz_targets/fuzz_object_trackers.rs` を変更不要にする。`skills/shiguredo-moqt/SKILL.md` は `ObjectFieldTracker` を import するだけで `observe_object_fields` を呼ばないため、変更不要である
- `ObjectFieldRecord` に immutables の生バイト列と payload_key を保持する (private)。immutables は Properties の長さに律速されるため生バイト列のまま保持する。payload_key は呼び出し側が算出した比較キーであり、固定長であることを doc で要求する
- 比較規則は「両方 `Some` のときだけ比較し、不一致なら `ObjectFieldMismatch` を返す」とする。片方でも `None` なら比較しない (見逃し側に倒す)
- crate 内にハッシュ関数を追加しない。payload の内容の比較キーの算出は呼び出し側の責務とし、ダイジェストを使う場合は衝突したときに「不一致と判定されない」(見逃しになる) ことを doc に明記する。Malformed Track の誤検出で正常な track を落とすより見逃し側に倒す
- Session は `accept_subgroup_object` と `recv_object_datagram` の `observe_object` 呼び出しで `observe_object_fields_with_content` を使う
  - immutables には `properties_bytes` から得た内側の生バイト列を渡す
  - payload_key は、subgroup 経路では `DecodedSubgroupObject::payload_length` と `status` を連結した固定長のバイト列、datagram 経路では `ObjectDatagram::status` だけの固定長のバイト列にする
  - `ObjectDatagram` は長さを保持せず (`remaining_payload_len` は decode 内で消費される)、status ありの datagram は payload 長 0 が wire で強制されるため、datagram では payload の内容を Session から比較できない
  - Session の公開受信 API (`recv_subgroup_object` / `recv_object_datagram`) のシグネチャは変えない
- 違反時は既存と同じ `ObjectFieldMismatch` を返し、Session の malformed track 経路 (`terminate_malformed_track`) に乗せる。新しいエラー型は増やさない
- **本 issue は 0111 の実装後に着手する**。0111 は `ObjectFieldMismatch` を返す §12.1 条件 6 / 7 の検出を `MessageError::MalformedTrack` 側に分類し、`Session::recv_subgroup_object` / `Session::recv_object_datagram` の戻り値を `RecvDataStreamError` に統一する。本 issue は 0111 の分類を変更しない
- 保持量の増加を doc に明記する。payload_key は固定長だが、immutables は生バイト列のため追跡中の Object 数 × immutables 長に比例する
- `ObjectFieldTracker` と `src/session/data.rs` の「検出しない」というコメントを実装に合わせて書き換える。何を Session で検出でき、何を payload のバイト列を持つ層に委ねるのかをコメントに残す
- `CHANGES.md` の `## develop` に `[ADD]` エントリを追加する (公開メソッドの追加であり後方互換)

## 完了条件

- 同じ (Group, Object) を異なる immutable properties で `ObjectFieldTracker` に観測させると `ObjectFieldMismatch` が返ることを固定するテストが追加されていること
- 同じ immutables を持つ重複 Object は受理されることを固定するテストが追加されていること (非退行)
- Session 経由 (subgroup / datagram のいずれか 1 経路以上) で payload_length または status が異なる重複 Object を受信すると、malformed track として subscription が終端されることを固定するテストが追加されていること
- `payload_key` を両方 `Some` で渡したときに、値が異なれば `ObjectFieldMismatch` が返ることを固定するテストが追加されていること。同じ長さで内容だけが異なる payload を、payload のバイト列を持つ層が算出したキーで検出できることも固定する
- `payload_key` の片方または両方が `None` のときは比較されず受理される (見逃し側に倒れる) ことを固定するテストが追加されていること
- Forwarding Preference / Subgroup ID / Priority の既存検出が非退行であること。既存の `observe_object_fields` のシグネチャと既存呼び出し元 (tests / pbt / fuzz) が変更不要であること
- Session が検出する範囲 (immutables と payload の長さ・status) と委ねる範囲 (payload の内容)、片方 `None` の扱い、ダイジェストの衝突時の見逃し、保持量の増加が doc コメントに明記されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
