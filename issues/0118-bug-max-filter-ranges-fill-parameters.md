# MAX_FILTER_RANGES が FILL_PARAMETERS 内側の Range を数えない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
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
