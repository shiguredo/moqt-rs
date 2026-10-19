# SubgroupStreamDecoder / FetchStreamDecoder の consume_payload 契約を明確化する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-consume-payload-contract
- Polished: {YYYY-MM-DD}

## 目的

`SubgroupStreamDecoder` / `FetchStreamDecoder` の `consume_payload` は、モジュール doc の疑似コードでは「データを push した後に呼ぶ」形で案内されるが、実装は `self.buf` を drain せず状態だけを戻す。`try_read_payload` との使い分けが利用者に伝わらず、誤用するとペイロード先頭を次の Object ヘッダとして解釈する。公開 API として契約を明確にし、未テストの経路をなくす。

## 現状

- `src/stream/decoder.rs` の `consume_payload` は `ConsumingPayload` 状態のとき `remaining` と一致すれば `AwaitingObject` に戻すだけで、`buf` を変更しない。doc には「呼び出し側が外部で読み出した場合に呼ぶ」とあるが、モジュール doc の疑似コードは `decoder.push(&data)` の後に `consume_payload` を呼ぶ形になっている。
- `try_read_payload` の後に `consume_payload` を呼ぶと「unexpected state」でエラーになる。逆に `consume_payload` だけを呼ぶとペイロードがバッファに残る。
- `consume_payload` は tests / pbt / fuzz から一度も呼ばれていない。
- `README.md` と `skills/shiguremoq/SKILL.md` は `try_read_payload()` / `consume_payload()` の両方を利用方法として案内している。
- 影響: 公開 API の誤用で誤デコードが起こり得る。テストで守られていない。

## 設計方針

- `consume_payload` を削除して `try_read_payload` に一本化する案と、バッファ drain と状態遷移を不可分にした API に置き換える案を比較し、Sans-I/O の使い勝手から決める。削除する場合は破壊的変更として CHANGES.md 相当の扱いにする。
- 残す場合は「ペイロードを decoder に push してはならない」契約を doc に明記し、バッファが空でない場合の呼び出しを `ProtocolViolation` にする。
- README / SKILL / モジュール doc の疑似コードを実態に合わせる。

## 完了条件

- 正常系・状態違い・長さ不一致の 3 経路がテストで固定されていること。
- README / SKILL / モジュール doc の案内が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
