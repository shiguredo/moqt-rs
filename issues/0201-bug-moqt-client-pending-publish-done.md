# moqt_client が保留中の PUBLISH_DONE を送信できない経路がある

- Created: 2026-10-06
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moqt-client-pending-publish-done
- Polished: 2026-10-06
- Updated: 2026-10-09

## 目的

`Session::forget_subscription` は、保留中の PUBLISH_DONE (`Subscription::pending_publish_done`) が
ある間は呼んではならない (順序契約として `Session::forget_subscription` の doc と
`skills/shiguredo-moqt/SKILL.md` に明記されている)。呼ぶと PUBLISH_DONE が送信されず、
draft-ietf-moq-transport-22 §9.5.1 (Updating Subscriptions) の MUST
(REQUEST_UPDATE が失敗した場合、publisher は PUBLISH_DONE を送る) を果たせない。

example の `MoqtClient` には、この MUST を阻害する経路が 2 つある。

- `cleanup_closed_requests` が `subscription_cleanup_ready` だけで subscription を破棄するため、
  保留中の PUBLISH_DONE が失われる
- `next_event` が `pump_once` (drain) より先に `take_notable_event` を呼び、Session のキューにある
  非 notable イベント (`SendOnStream`) を捨てるため、data plane が同期 API で flush した
  PUBLISH_DONE がワイヤへ出ない

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `cleanup_closed_requests` は
  `session.subscription_cleanup_ready(request_id) == Some(true)` のとき `forget_subscription` を呼ぶ
- `Subscription::cleanup_ready` (`src/session/types.rs`) は受信側の状態 (`state == Terminated`、
  `publish_done` の drain 満期 / Stream Count 超過、`open_incoming_subgroup_count == 0`) だけを見ており、
  `pending_publish_done` と open 中の送信 stream を見ない。そのため publisher 役の
  REQUEST_UPDATE 失敗応答で保留した PUBLISH_DONE を持つ subscription でも true になり、
  open 中の送信 stream が残っていても破棄されうる
- 現状の example の REQUEST_UPDATE 処理 (`examples/moq-pub/src/pipeline.rs` と
  `examples/moq-sub/src/pipeline.rs`) は常に REQUEST_OK を返すため、保留が発生する経路には到達しない。
  ただし `MoqtClient::send_request_error` は公開されており、失敗応答にする経路を追加すると到達する
- `Session::forget_subscription` は `subscriptions` から entry を除去したうえで `pending_publish_done` も
  破棄するため、破棄後に全 outgoing stream を閉じても `Session::maybe_flush_pending_publish_done` は
  entry を見つけられず PUBLISH_DONE を送信しない
- `Session::maybe_flush_pending_publish_done` は `SendOnStream` をイベントキューへ push して
  `pending_publish_done` を `take` するだけで、ワイヤへの書き出しは `MoqtClient::drain_events` が行う。
  `MoqtClient::tick` は Session のイベントを drain せずに `cleanup_closed_requests` を呼ぶため、
  flush の直後に tick が来ると、push 済みで未送出の PUBLISH_DONE を `bidi_sends` の除去で捨ててしまう
  (`drain_events` の `SendOnStream` 分岐が `Dropping send on already closed request stream` を warn する経路)
- `MoqtClient::next_event` はループの先頭で `take_notable_event` を呼び、その後で `pump_once` を呼ぶ。
  `take_notable_event` は `notable_events` が空のとき Session のイベントキューを `poll_event` で
  直接 pop し、`is_notable_event` に該当しないイベントを捨てる。`SendOnStream` は notable ではないため、
  data plane (`DataPlaneHandle::send_data_stream_closed`) のように drain を伴わない同期経路で積まれた
  `SendOnStream` は、次の `next_event` の `take_notable_event` で捨てられてワイヤへ出ない
  (保留 PUBLISH_DONE の flush も `send_data_stream_closed` から `maybe_flush_pending_publish_done` を
  通るため、この経路で失われる)

## 設計方針

- `cleanup_closed_requests` の subscription の破棄条件に「保留中の PUBLISH_DONE が無いこと」を加える。
  判定には `Session::subscription` が返す `Subscription::pending_publish_done` を使う
- 破棄の実行契機を Session のイベントを drain した後に限定する。flush は `SendOnStream` を push するだけで
  書き出しは `drain_events` が行うため、drain 前に破棄すると PUBLISH_DONE が未送出のまま失われる。
  `MoqtClient::tick` は sync で drain できないため、`tick` からの `cleanup_closed_requests` 呼び出しをやめ、
  drain 経路 (`drain_events` の末尾と `pump_once` の bidi Closed 分岐) に一本化する
- `next_event` は `take_notable_event` より先に Session のイベントを drain する (`drain_events` を呼ぶ)。
  あわせて `take_notable_event` から Session のキューを poll するフォールバックを外し、`notable_events` のみを
  pop する形にする。notable イベントの配送は `drain_events` が担い、キューに積まれた `SendOnStream` は
  I/O 操作へ変換されるため捨てられなくなる (notable イベントの返却は drain の後ろに回る)
- `drain_events` の `CloseSession` 分岐は transport を閉じたうえで同じイベントを `notable_events` へ push し、
  `ClientEvent::Session(CloseSession)` としてアプリへ届くことを維持する。現行は `take_notable_event` の
  Session poll だけが配送経路であり、drain が先に消費すると終了理由の観測が失われる
  (0126 が確立した契約。0099 は `CloseSession` を notable キューの対象外とし、`drain_events` の
  transport close を前提にしている)
- 保留中の PUBLISH_DONE が `Some` の間は flush されない (`maybe_flush_pending_publish_done` が open 中の
  送信 stream がある間は早期 return する) ため、flush による `None` への変化は open 中の送信 stream が
  無いときにだけ起こる。ただし `pending_publish_done` が `None` でも open 中の送信 stream が残りうる
  (保留が一度も無い subscription)。その場合の扱いは本 issue では変えない (現行どおり `cleanup_ready` に従う)
- `drain_events` の notable 判定は `is_notable_event` を使う形に寄せる。`take_notable_event` の poll を外すと
  非テストビルドで `is_notable_event` が未使用になり、`-D warnings` の clippy が失敗するため。
  あわせて `take_notable_event` / `is_notable_event` の doc と `drain_events` 内のコメントを
  新しい配送契約に合わせて更新する
- 破棄しなかった request は `closed_request_streams` に残し、flush と送出の後の cleanup で破棄する
- fetch / track_status には保留 PUBLISH_DONE に相当する状態が無いため、条件の追加は subscription だけにする
- 判定は `examples/tokio-moq/src/moqt_client.rs` の純関数として切り出す (例:
  `fn should_forget_subscription(cleanup_ready: bool, pending_publish_done: Option<u64>) -> bool`)。
  `Subscription` 全体を引数にすると 28 個の pub フィールドをテストで組むことになるため、判定に必要な値だけを渡す。
  example 内で完結するため `pub(crate)` にはしない
- ログは変えない (未送出の PUBLISH_DONE を捨てる warn 経路を新たに作らず、`CloseSession` の終了理由の
  ログも維持する)

## 完了条件

- 保留中の PUBLISH_DONE を持つ subscription では破棄の述語が false を返し、保留が無ければ
  `cleanup_ready` に従うこと (`examples/tokio-moq/src/moqt_client.rs` の `#[cfg(test)] mod tests` に
  モックやスタブを使わない単体テストを追加して固定する)
- Session のキューに積まれた `SendOnStream` が `next_event` で捨てられず、flush された PUBLISH_DONE が
  request stream へ書き出されること (`next_event` が drain を先に行い、`take_notable_event` は
  `notable_events` のみを pop する。example の配線は I/O ハンドルが必要なため単体テストの対象外とし、
  `next_event` / `take_notable_event` の変更をレビューで確認する)
- `drain_events` が消費した `CloseSession` も `notable_events` 経由で `ClientEvent::Session(CloseSession)`
  としてアプリへ届き、moq-pub / moq-sub の終了理由のログが変わらないこと (同上、レビューで確認する)
- flush で push された PUBLISH_DONE が request stream へ書き出された後に subscription が破棄されること
  (`cleanup_closed_requests` が drain 後にだけ呼ばれ、未送出の `SendOnStream` を破棄しないこと。同上)
- 破棄しなかった request が `closed_request_streams` に残り、次の cleanup で破棄されること (同上)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
