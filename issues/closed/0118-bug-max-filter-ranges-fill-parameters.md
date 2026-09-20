# MAX_FILTER_RANGES が FILL_PARAMETERS 内側の Range を数えない

- Created: 2026-09-21
- Completed: 2026-09-22
- Branch: feature/fix-max-filter-ranges-fill-parameters
- Polished: 2026-09-21

## 目的

draft-ietf-moq-transport-21 §9.1.6 (MAX FILTER RANGES) と §3.3.2 (Range Filters) は、MAX_FILTER_RANGES が「ある subscription / fetch に対する全ての Range Filter パラメータ内の Range 総数」を制限し、超過時は INVALID_FILTER の REQUEST_ERROR で拒否する MUST を定める。

> The MAX_FILTER_RANGES option (Type 0x06) limits the peer's total number of Ranges (Start/End pairs) allowed concurrently in all Range filter Section 3.3.2 parameters for a given subscription or fetch.
> The default value is 0, so if not specified, the peer MUST NOT send any such filter parameters.
> If this limit is exceeded, an endpoint MUST reject this with REQUEST_ERROR with error code INVALID_FILTER.

§9.20.16 (FILL PARAMETERS Parameter) は FILL_PARAMETERS の内側に Range Filter (0x25-0x28) を置けるとしている。現状は外側のリストしか数えないため、上限を超えるフィルタを送信・受理しうる。

## 現状

- `src/message_parameter.rs` の `MessageParameters::count_range_filters` は自リスト (`self.0`) の Range Filter のみを数える。`MessageParameters::has_range_filters` も自リストのみを見る
- `src/session/core.rs` の `Session::check_incoming_range_filters` は早期 return の条件に内側の有無 (`parameters.fill_parameters().is_some_and(|fill| fill.has_range_filters())`) を含めるが、上限比較は `parameters.count_range_filters()` のままである。
  このため内側だけに Range がある場合 `count` は 0 になり、`MAX_FILTER_RANGES = 0` でも上限判定を通過する。上限 1 で「外側 1 個 + 内側 1 個」の合計 2 個も通過する
- `src/session/core.rs` の `Session::validate_outgoing_range_filters` も同じ構造である。`peer_max == 0` の判定は内側の有無も見るため「内側のみ」は拒否するが、上限比較は外側のみなので「外側 1 個 + 内側 1 個、peer_max 1」は通過する。内側の扱いが 0 判定と上限判定で非対称になっている
- subscription 単位の累積検証 (`src/session/subscription/recv.rs` の `handle_update_for_subscription` にある `merged.count_range_filters() > self.local_max_filter_ranges()`) も同じ関数を使うため、内側の Range は累積に数えられない。ガード条件 `merged.has_range_filters()` も外側しか見ないため、外側に Range が無く内側だけに Range がある REQUEST_UPDATE は上限判定に入らない
- FILL_PARAMETERS は subscription state として保持されない (§9.20.16 "FILL_PARAMETERS is not retained as subscription state.")。`handle_update_for_subscription` も `pending.remove_type(PARAM_FILL_PARAMETERS)` で内側を保持しない
- 内側を数えないことは偶然ではなく、`Session::check_incoming_range_filters` のコメントに意図として書かれている (「内側は運搬メッセージにのみ適用される transient なスコープのため、MAX_FILTER_RANGES の累積数には含めない」)。ただし同じコメント群が「FILL 内側の Range Filter も検証対象に含める」とも書いており、有無の判定と総数の判定で規則が食い違っている
- `tests/test_session/subscription/subscription_limits.rs` の MAX_FILTER_RANGES 節は外側リストのみを対象にしており、外側 + 内側の合計を固定したテストは無い

## 設計方針

- §9.20.16 の "separate parameter scope" は §9.20 の重複判定のための規定であり、§3.3.2 の予算には及ばないと解釈する。引用文自身が効力範囲を "for the purposes of Section 9.20" に限定している。

  > The value of FILL_PARAMETERS is a separate parameter scope. Parameters inside it are not considered to appear in the enclosing message for the purposes of Section 9.20, so a Parameter Type MAY appear both in the message and inside FILL_PARAMETERS.

- 内側の Range Filter も subscription のフィルタの一部である。§9.20.16 は FILL_PARAMETERS の値を「fill fetch stream に適用される Parameters の列」と定め、同節の表で 0x25-0x28 を Section 3.3.2 (Range Filters) として内側に置いている。

  > Its value is a sequence of Parameters that apply to the fill fetch stream (see Section 3.4), encoded as if they were Parameters for a separate message (see Section 16.7).

  したがって「a given subscription or fetch の全ての Range Filter」に内側も含まれる。内側の Range も総数に合算する
- 合算の規則は session 層の private な helper 1 箇所に置く。`MessageParameters::count_range_filters` / `has_range_filters` の意味は変えない
- 両者は公開 API (`pub mod message_parameter`) であり、`src/session/subscription/dispatch.rs` の `send_ok_for_subscription` が `pending_params.has_range_filters()` を subscription の `range_filters` 再構築という別用途で使っている。意味を変えると後方非互換になる
- 合算値は「自リストの Range Filter が持つ Range の総数 + `fill_parameters()` の内側の Range の総数」とする。有無の判定と総数の判定は同じ helper を使う
- 送信側 (`Session::validate_outgoing_range_filters`)、受信側 (`Session::check_incoming_range_filters`)、累積検証 (`Session::handle_update_for_subscription`) の 3 経路すべてで同じ合算値を使う
- 累積検証のガード条件を合算の有無に変える。現行の `merged.has_range_filters()` は外側しか見ないため、外側に Range が無く内側だけに Range がある REQUEST_UPDATE が上限判定に入らない
- 累積値は「保持済みの外側パラメータの合計 + 今回メッセージの内側の合計」とする。§9.20.16 は "FILL_PARAMETERS is not retained as subscription state." と定めており、`Session::handle_update_for_subscription` も `pending.remove_type(PARAM_FILL_PARAMETERS)` で内側を保持しない。前回メッセージの内側を次の REQUEST_UPDATE の累積に数え直さない
- 「Range を持たない Range Filter インスタンスを MAX_FILTER_RANGES = 0 の受信で受理する」という既存の意図的な非対称 (§3.3.2 に受信側の拒否 MUST が無いことによる) は変えない。本 issue は総数の数え方だけを変える
- FILL_PARAMETERS を運べるのは SUBSCRIBE と REQUEST_UPDATE (subscription 用) だけである (§9.20.16)。FETCH は FILL_PARAMETERS を運べず内側が存在しないため合算の対象外であり、FETCH の外側 Range Filter は従来どおり `MAX_FILTER_RANGES` の上限検証の対象である
- `Session::check_incoming_range_filters` の「内側は累積数に含めない」というコメントを、上記の解釈と実装に合わせて書き換える (「累積パラメータからは除外される」という FILL_PARAMETERS 非保持の記述は事実のまま残す)

## 完了条件

- 上限 1 のとき、外側 1 個 + 内側 1 個の Range Filter を持つ SUBSCRIBE / REQUEST_UPDATE の送信が API で拒否されること
- 同じ入力を受信したとき INVALID_FILTER の REQUEST_ERROR で拒否されること
- 外側 + 内側の合計が上限以内 (境界: 合計 = 上限) なら送受信とも受理されること
- 外側に Range が無く内側だけに Range がある REQUEST_UPDATE でも、累積検証の上限判定が働くこと (ガード条件の修正)
- 累積値が「保持済みの外側の合計 + 今回メッセージの内側の合計」で数えられ、前回メッセージの内側が次の REQUEST_UPDATE で二重計上されないこと
- FETCH は FILL_PARAMETERS を運べないため合算の対象外であり、FETCH の外側 Range Filter が従来どおり `MAX_FILTER_RANGES` の上限検証の対象であることがテストで固定されていること
- 「Range を持たない Range Filter インスタンスは MAX_FILTER_RANGES = 0 の受信で受理される」既存挙動が変わらないこと
- `MessageParameters::count_range_filters` / `has_range_filters` の公開 API の意味が変わっていないこと
- `cargo test --workspace` が通ること

## 解決方法

session 層に private な `range_filter_usage(parameters) -> (bool, u64)` を追加し、外側の Range Filter の Range 総数と FILL_PARAMETERS (`fill_parameters()`) の内側の総数を合算するようにした。戻り値は「Range Filter パラメータが 1 つでもあるか」と「Range の総数」である。

1. §9.20.16 の "separate parameter scope" は §9.20 の重複判定のための規定であり、効力範囲は引用文自身が "for the purposes of Section 9.20" に限定しているため §3.3.2 の予算には及ばない、という解釈を helper の doc に明記した。§9.20.16 が内側に置けるのは 0x25-0x28 であり (0x29 TRACK_PROPERTY_FILTER は Table 6 に無い)、内側の Range は同じ subscription の予算に含まれる
2. 合算は session 層の helper 1 箇所に閉じた。公開 API である `MessageParameters::count_range_filters` / `has_range_filters` の意味は変えていない (`src/session/subscription/dispatch.rs` が subscription の `range_filters` 再構築という別用途で外側だけを見るために使うため)
3. 送信 (`Session::validate_outgoing_range_filters`)・受信 (`Session::check_incoming_range_filters`)・subscription 単位の累積検証 (`Session::handle_update_for_subscription`) の 3 経路すべてで同じ helper を使う。「内側だけに Range がある」入力でも有無の判定が真になり上限判定に到達する
4. 累積検証の値は「マージ後の外側の Range 総数 (同一型は今回のメッセージの値で置換済み) + 今回メッセージの内側の Range 総数」になる。`pending_update_params` からは FILL_PARAMETERS を除去しているため前回メッセージの内側を二重計上しない (§9.20.16 "FILL_PARAMETERS is not retained as subscription state.")
5. 累積検証のガード条件は合算の有無を使う形にしたが、**Range 総数が 1 以上なら Range Filter パラメータも必ず存在するため、この変更自体は受理・拒否の結果を変えない**。実際に挙動を変えるのは総数の合算であり、ガードは単体検証と同じ合算値を使うための対称性のためのものである (累積判定は `accumulated_ranges > local_max` のみで足りる)
6. `validate_outgoing_range_filters` の doc から、本ライブラリが実装しない SUBSCRIBE_TRACKS を送信経路の列挙から外し、応答経路 (REQUEST_OK 系) からも呼ばれるが実質 no-op である旨を補足した
7. FETCH は FILL_PARAMETERS を運べない (`FETCH_ALLOWED_PARAMS` に含まれない) ため合算の対象外であり、外側の Range Filter は従来どおり上限検証の対象である

テスト:

- `tests/test_session/subscription/subscription_limits.rs` に次を追加した
  - 上限 1 で外側 1 + 内側 1 の REQUEST_UPDATE / SUBSCRIBE の送信拒否 (理由文字列まで断言) と受信拒否 (INVALID_FILTER の REQUEST_ERROR)
  - 外側に Range Filter が無く内側にだけ Range がある入力の送信拒否 (peer 未宣言 / 上限超過) と受信拒否 (自側 MAX=0 / MAX=1)
  - 合計が上限と等しい入力の受理 (送信・受信とも) と、内側にだけ Range 1 個の受理
  - 外側に Range が無く内側だけの REQUEST_UPDATE でも累積上限が働くこと (保持済みの外側 2 + 内側 2 > 上限 2)
  - 前回メッセージの内側を累積に数え直さないこと (1 通目 = 内側 1、2 通目 = FILL なし + 外側 1 で受理)
  - FILL 内側の Range Filter が 0 個の 3 形態を MAX_FILTER_RANGES=0 の受信と MAX=1 の送信で受理すること (既存挙動の保全)
  - FILL 内側の Range Filter の構造不正 (重複) が上限内でも送受信とも拒否されること (従来は上限判定で先に拒否されていたため構造検証が未到達だった)
  - FETCH の外側 Range Filter が上限検証の対象であること
- 各テストは変異実験で「対応する実装を壊すと落ちる」ことを確認した (helper の有無判定を外側のみ / 総数から内側を落とす / 内側を二重計上 / FILL を pending に保持 / 受信と送信の FILL 内側構造検証を削除 / 累積の総数を外側のみ、の各変異で対応テストが失敗する)
- `MessageParameters::count_range_filters` が外側だけを数えることをテストで断言し、公開 API の意味が変わっていないことを固定した

`CHANGES.md` の `## develop` に `[FIX]` を追加した。
