# MoqtClient の drain が future の破棄でメッセージを失う

- Created: 2026-10-09
- Completed: 2026-10-11
- Branch: feature/fix-moqt-client-drain-cancel-safety
- Polished: 2026-10-09
- Updated: 2026-10-11

## 目的

`MoqtClient::next_event` は moq-pub / moq-sub で `tokio::select!` の分岐 future として使われる。分岐が
負けると future は await の途中で破棄されるため、内部で呼ぶ `drain_events` の送信処理が中断される。
`bidi_sends` から送信半を取り出した後・送信完了前に破棄されると、送信半が台帳から消えたままメッセージが
送られず、以後の `SendOnStream` が「Dropping send on already closed request stream」で捨てられる。
保留中の PUBLISH_DONE (`UPDATE_FAILED`) を失うと §9.5.1 (Updating Subscriptions) の MUST を果たせない。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `drain_events` の `SessionEvent::SendOnStream` 分岐は
  `bidi_sends` から送信半を取り出してから `send.send(...).await` する。`fin: false` の場合は送信後に
  台帳へ戻すが、await 中に破棄されると再登録に到達しない。
- 同じ `drain_events` の `SessionEvent::SendRequest` 分岐も `open_bidi_stream().await` と
  `send.send(...).await` の途中で破棄されると、開いたストリームが台帳へ登録されず受信タスクも
  起動されないまま失われる。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` は
  `notable = client.next_event() =>` の分岐で `next_event` を poll しており、他の分岐が
  先に完了すると破棄される。moq-pub の他の分岐は映像 / 音声入力、tick、catalog の再送、
  shutdown であり、moq-sub では data stream の accept、catalog の更新、tick、終了依頼
  (termination)、shutdown、player の停止が該当する (moq-sub では data stream の accept が
  頻繁に成立するため、破棄自体は珍しくない)。
- 送信半を台帳から取り出した後の `send.send(...)` がエラーで返った場合 (`?` で呼び出し側へ
  伝播し、この時点では警告ログは出ない) も送信半は台帳へ戻されない。以後の `SendOnStream` は
  「Dropping send on already closed request stream」を warn して捨てられる。
- 0201 で「捨てられるイベントを減らす」方向の修正は入ったが、future 破棄そのものは未解消。
- 現行の examples (moq-pub / moq-sub) の REQUEST_UPDATE 処理は常に REQUEST_OK を返すため、
  保留 PUBLISH_DONE の flush 送信が破棄される経路には現状到達しない (公開 API の
  `send_request_error` を REQUEST_UPDATE の失敗応答に使う配線で到達する。moq-pub は
  SUBSCRIBE / FETCH の失敗応答に `send_request_error` を既に使っているが、保留 PUBLISH_DONE が
  立つのは Established の購読を拒否した場合だけである)。一方、peer 起点の要求への自動 REQUEST_ERROR
  送信 (`Session::emit_request_error`) は `pump_once` 経由で drain されるため、送信途中の
  破棄で応答が失われる経路は現行でも存在する。

## 設計方針

- 破棄されても台帳とメッセージが壊れない形にする。案を比較して決める。
  - 送信半を台帳に残したまま `&mut` で送信し、FIN のときだけ完了後に取り出す
  - 送信中の状態を台帳に持たせ、破棄時に復元する (Drop 実装は使わず、状態を明示的に持つ)
  - `drain_events` を専用タスクへ移し、`next_event` の破棄の影響を受けないようにする
    (2026-10-10 以降の moq-sub の pipeline には「s2n-quic のエンドポイントは `next_event()` の poll で
    動く」ため `select!` の分岐内で待ってはならない旨が記録されている。専用タスク化は
    エンドポイントと session タイマーの駆動主体を変えるため、成立可否を確認してから採否を決める)
- 各 transport の送信 API が途中で破棄された場合にどこまで書かれるか (cancel safety) を確認する。
  部分送信が起きる場合はメッセージ単位の再送ができないため、その限界を doc に明記する。
- 送信失敗 (`?`) の経路でも台帳の状態と警告の有無を揃える。
- FIN / RESET 済みの request を no-op にする既存挙動は変えない。

## 完了条件

- `next_event` の future が破棄されても送信半が台帳から消えないこと。破棄後に再度
  `next_event` を呼ぶと、破棄時点でまだ送信を開始していなかったメッセージ (Session の
  イベントキューに残っていたもの) が送られること。
- 送信中に破棄されたメッセージの扱い (再送するか、部分送信により再送不能となるか) が
  doc に明記されていること。
- 追加した挙動を単体テストで固定すること (I/O ハンドルが必要な配線部分はレビューで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

## 解決方法

`drain_events` の `SendOnStream` で送信半を台帳から取り出す順序を変え、送信中は `bidi_sends` に
残したまま `get_mut` で送信し、FIN を送るときだけ送信の完了後に取り出すようにした。これにより
`next_event` の future が await の途中で破棄されても送信半が失われず、破棄後に呼び直すと Session の
イベントキューに残っていたメッセージから送信が再開される。FIN の送信に失敗した場合は送信半を
台帳へ戻して warn し、以後の送信で再試行できるようにした。

`SendRequest` は encode → ストリームを開く → 受信タスクと STOP_SENDING のチャネルを登録 →
ヘッドメッセージを送信 → 台帳へ登録、の順にした。台帳への登録を送信より先にすると、破棄時に
ヘッド未送信のストリームが台帳へ残り、以後の応答や PUBLISH_DONE が初回メッセージとしてワイヤに
載って peer が PROTOCOL_VIOLATION でセッションを閉じるためである。

送信中に破棄されたメッセージの扱いと、破棄で失われる 3 経路 (ストリームを開く await、ヘッド
メッセージの送信、確立済み request のメッセージの送信) を `drain_events` の doc に明記した。送信の
3 経路 (QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2) の実装を確認し、いずれも
メッセージ単位で書き込むため部分送信は起きないこと、破棄されたメッセージが送られるかどうかは
transport に依存することを記録した。

`SendOnStream` の判定は `plan_send_on_stream` に切り出し、送信半が無い request では encode も
送信もしないこと (FIN / RESET 済みの request を no-op にする既存挙動の維持) と、`fin` のときだけ
送信の完了後に台帳から取り出すことを単体テストで固定した。I/O ハンドルが要る配線の順序は
差分レビュー 3 周で確認した。回収時の FIN 失敗も warn するようにした。

`cargo test --workspace` (2645 件) / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` はすべて通っている。
