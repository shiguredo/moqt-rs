# SubgroupStreamDecoder / FetchStreamDecoder の consume_payload 契約を明確化する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-consume-payload-contract
- Polished: 2026-10-20

## 目的

`SubgroupStreamDecoder` / `FetchStreamDecoder` の `consume_payload` は、モジュール doc と `try_decode_object` /
`try_decode_entry` の doc が「バッファからペイロードを読み出した後に呼ぶ」と案内する一方、実装は `self.buf` を drain せず状態だけを戻す。`consume_payload`
自身の doc は「呼び出し側が外部で読み出した場合に呼ぶ」と書いており、案内が矛盾している。`try_read_payload` との使い分けが利用者に伝わらず、誤用するとペイロード先頭を次の Object
ヘッダとして解釈する。公開 API の契約を「外部読み出し専用」に厳密化し、誤用を `ProtocolViolation` で検出できるようにする。あわせて未テストの経路をテストで固定する。

## 現状

- `src/stream/decoder.rs` の `SubgroupStreamDecoder::consume_payload` は `ConsumingPayload { remaining }` のとき `length == remaining` なら `AwaitingObject` に戻すだけで、`buf` を変更しない。`FetchStreamDecoder::consume_payload` も同様に `AwaitingEntry` に戻すだけで `buf` を変更しない。
- `SubgroupStreamDecoder` のモジュール doc は次のとおり案内する。

  > ペイロードはデコーダの責務外。`try_decode_object` が返す `payload_length` バイト分を呼び出し側がバッファから消費した後、`consume_payload` を呼んで次のオブジェクトに進む。

  `try_decode_object` の doc も次のとおり案内する。

  > 呼び出し側は `payload_length` バイト分のペイロードをバッファから読み出した後、`consume_payload` を呼ぶこと。

  `FetchStreamDecoder` のモジュール doc と `try_decode_entry` の doc も、`try_decode_entry` と `次のエントリ` に対応する「バッファから読み出した後 `consume_payload`」の案内をする。
- 一方、`consume_payload` 自身の doc は次のとおり案内する。

  > `try_decode_object` が返した `payload_length` バイト分のペイロードを呼び出し側が外部で読み出した場合に呼ぶ。バッファ内のペイロードを読み出す場合は `try_read_payload` を使う。

  `FetchStreamDecoder::consume_payload` の doc も同じ契約を書く。モジュール doc・`try_decode_object` / `try_decode_entry` の doc・疑似コードは、この「外部で読み出す場合」の契約と矛盾する。
- `try_read_payload` の後に `consume_payload` を呼ぶと `ProtocolViolation("consume_payload called in unexpected state")` になる。逆に、ペイロードを decoder に push したまま `consume_payload` だけを呼ぶと、状態は `AwaitingObject` / `AwaitingEntry` に戻るが `buf` にペイロードが残り、次の `try_decode_object` / `try_decode_entry` がペイロード先頭を次の Object ヘッダとして解釈する。
- `consume_payload` は `src/`、`examples/`、`tests/`、`pbt/`、`fuzz/` のいずれからも呼ばれていない。`rg -n "consume_payload" src/ examples/ tests/ pbt/ fuzz/` のヒットは `src/stream/decoder.rs` の定義・実装と doc コメント、`tests/test_stream/decoder.rs` のコメントだけで、呼び出しは無い。
- `README.md` の `SubgroupStreamDecoder` の説明は「ペイロードは `try_read_payload()` / `consume_payload()` で進める」と両方を並べ、`skills/shiguredo-moqt/SKILL.md` のデコーダ API 一覧と説明も両方を案内する。
- `tests/test_stream/decoder.rs` の `encode_subgroup_stream` の doc コメントは「`consume_payload` 後にバッファから drain する必要があるためテスト側で処理する」と書くが、実際のテストは `try_read_payload` で drain しており、コメントが実装と一致しない。
- 影響: 公開 API の誤用で誤デコードが起こり得る。`consume_payload` の正常系・長さ不一致・状態違い・バッファ非空の経路はテストで守られていない。

## 設計方針

- `consume_payload` は削除せず、外部読み出し専用の API として契約を厳密化する。`try_read_payload` は decoder のバッファに push されたペイロードを drain して状態を進める API であり、外部読み出しの用途を代替しない。`consume_payload` に drain を持たせると `try_read_payload` と重複し、外部読み出しの用途を失うため、この案は採らない。削除案も、外部読み出しの Sans-I/O 用途を失うため採らない（0329 も `consume_payload` を削除対象にしていない）。
- `consume_payload` の契約を次のとおり定める。
  - 呼び出し側がペイロードを外部で読み出した場合だけ呼ぶ。ペイロードを decoder に push してはならず、次のデータを push する前に呼ぶ。
  - 呼び出し時点で `buf` は空でなければならない。`buf` が空でない場合は `ProtocolViolation` を返し、状態を変えない。
  - `length` は `ConsumingPayload { remaining }` の `remaining` と一致しなければならない。一致しない場合は `ProtocolViolation` を返し、状態を変えない。
  - 状態が `ConsumingPayload` でない場合は `ProtocolViolation` を返し、状態を変えない。
  - 成功時は `SubgroupStreamDecoder` は `AwaitingObject`、`FetchStreamDecoder` は `AwaitingEntry` へ遷移する。
  - 判定順は状態 → 長さ → バッファの順とし、最初に違反した条件で `ProtocolViolation` を返す。
- `src/stream/decoder.rs` の `SubgroupStreamDecoder::consume_payload` / `FetchStreamDecoder::consume_payload` に、状態 → 長さ → バッファの順の検証を実装する。バッファが空でない場合は `ProtocolViolation` を返し、状態を変えない。
- `try_read_payload` の後に `consume_payload` を呼ぶのは誤用であり、状態違いの `ProtocolViolation` になる。`payload_length == 0` の Object と EndOfRange エントリでは `consume_payload` を呼ばない。これらを doc に明記する。
- `src/stream/decoder.rs` の `SubgroupStreamDecoder` / `FetchStreamDecoder` のモジュール doc、`try_decode_object` /
  `try_decode_entry` の doc、`consume_payload` の doc、疑似コードを更新する。モジュール doc の主な疑似コードは `try_read_payload` を使う形にし、
  `consume_payload` の外部読み出し例を別に示す。`try_decode_object` / `try_decode_entry` の doc は「バッファから読み出す場合は
  `try_read_payload`、外部で読み出す場合は `consume_payload`」と書き分ける。
- `README.md` の `SubgroupStreamDecoder` の説明と `skills/shiguredo-moqt/SKILL.md` のデコーダ API 一覧・説明を、上記の使い分けに合わせる。0324（docs / README / SKILL.md を実装に合わせる）と 0321（公開 API の doc を整備する）は、本 issue の契約変更に伴う該当箇所を対象外とし、本 issue で更新する。
- `tests/test_stream/decoder.rs` の `encode_subgroup_stream` の doc コメントを、`try_read_payload` が drain する実装に合わせる。
- `tests/test_stream/decoder.rs` に `SubgroupStreamDecoder` / `FetchStreamDecoder` の両方で次の 4 経路を追加する。
  - 正常系: ヘッダーと最初の Object ヘッダーだけを push し、ペイロードを push せずに `consume_payload(payload_length)` を呼ぶと成功する。その後、次の Object ヘッダーを push して次の Object をデコードできる。
  - 長さ不一致: 誤った長さで呼ぶと `ProtocolViolation` になり、状態が `ConsumingPayload` のまま残る。その後、正しい長さで呼ぶと成功する。
  - 状態違い: ヘッダー未デコード、`payload_length == 0` の Object の後、EndOfRange エントリの後、`try_read_payload` の後に呼ぶと `ProtocolViolation` になり、状態は変わらない。
  - バッファ非空: ペイロードを push した状態で `consume_payload(payload_length)` を呼ぶと `ProtocolViolation` になり、状態が `ConsumingPayload` のまま残る。その後 `try_read_payload` でペイロードを読み出せて、次の Object に進める。
- 契約の厳密化は公開 API の契約変更であり、誤用時の挙動が変わる。`CODEBASE.md` の「この指示がなくなるまでは変更履歴を `CHANGES.md` に残さないこと」に従い、`CHANGES.md` は更新しない。
- 0319 の `consume_payload` テスト項目（正常系・長さ不一致・状態違い）は本 issue の 4 経路テストと重複する。0319 側で本 issue のテストに合わせて整理する。本 issue では 0319 を編集しない。

## 完了条件

- `consume_payload` の契約（外部読み出し専用、呼び出し時 `buf` が空、`length == remaining`、`ConsumingPayload` 状態、エラー時状態不変、成功時の遷移先）が `SubgroupStreamDecoder` / `FetchStreamDecoder` の doc に明記されていること。
- `consume_payload` が、`buf` が空でない場合・`length` が `remaining` と一致しない場合・`ConsumingPayload` 状態でない場合に `ProtocolViolation` を返し、状態を変えないこと。
- `SubgroupStreamDecoder` / `FetchStreamDecoder` の両方で、正常系・長さ不一致・状態違い・バッファ非空の 4 経路が `tests/test_stream/decoder.rs` のテストで固定されていること。
- `README.md`、`skills/shiguredo-moqt/SKILL.md`、`src/stream/decoder.rs` のモジュール doc・`try_decode_object` / `try_decode_entry` の doc・`consume_payload` の doc・疑似コードの案内が実装と一致すること。
- `tests/test_stream/decoder.rs` の `encode_subgroup_stream` の doc コメントが実装と一致すること。
- `CHANGES.md` を更新していないこと（`CODEBASE.md` の現方針）。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
