# request stream の先頭メッセージを種別に応じて拒否またはセッション終了にする

- Created: 2026-10-12
- Completed: 2026-10-13
- Branch: feature/fix-peer-request-start-message
- Polished: 2026-10-13

## 目的

peer が開始した request stream の先頭メッセージの扱いが、draft-ietf-moq-transport-22 と
合っていない。§6.3 (Session initialization) は先頭メッセージを 7 種類 (SUBSCRIBE / PUBLISH /
FETCH / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / TRACK_STATUS / SUBSCRIBE_TRACKS) に限り、
それ以外は PROTOCOL_VIOLATION でセッションを閉じるよう MUST で定める。また本実装が扱わない
3 種類 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) は §9 の Table 5 で
request stream の先頭として定義済みであり、§1.6 (Modularity) の "Limited endpoints SHOULD
respond to any unsupported messages with the appropriate NOT_SUPPORTED error code, rather than
ignoring them." に従って REQUEST_ERROR (NOT_SUPPORTED) で拒否しセッションを維持する。現状は
どちらも警告ログを出してメッセージを捨てるだけで、セッションの状態が変わらない。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `spawn_peer_bidi_accept_task` は、peer が開始した
  request stream の先頭メッセージを `request_id_of` で判定し、`None` の場合は `tracing::warn!` を
  出して return する (送信半も捨てる)。main ループへは伝えないため、session は Established の
  まま残る。
- `request_id_of` は `ControlMessage` の variant のうち 4 種類 (SUBSCRIBE / PUBLISH / FETCH /
  TRACK_STATUS) しか扱わない。本実装が扱わない 3 種類は `ControlMessage::Unsupported`
  (デコード済みの request 構造体を持たず、`type_id` と request なら `request_id`、生の `body` を
  保持する) として届くため、§6.3 の 7 種類に含まれるにもかかわらず同じ warn 破棄の経路に
  落ちている。
- decode 失敗は `BidiMessage::PeerRequestError` で main ループへ伝えてセッション終了に
  つなげている (0222 で対応)。先頭メッセージの種別判定だけが同じ扱いから漏れている。
- library の `Session::recv_request` は 7 種類以外を PROTOCOL_VIOLATION として closing に遷移
  させ、扱わない 3 種類は `REQUEST_ERROR` (NOT_SUPPORTED、control GOAWAY を送信済みなら
  GOING_AWAY) を積んでセッションを維持する。example はこれを呼ばずに破棄している。
- `Session::recv_request` の doc は「扱わない 3 種類 … と、7 種類以外の message type … は
  `PROTOCOL_VIOLATION` で session を closing に遷移させる」と書かれており、3 種類の扱いが実装と
  食い違っている。

## 設計方針

- example 側で行う判定は「アプリへ渡す 4 種類 (SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS) か、
  それ以外か」だけにする。§6.3 の 7 種類判定は library の `Session::recv_request` が既に持つ
  ため複製しない (3 種類を type_id で判定するには library の private / `pub(crate)` な定数が
  必要になり、example からは参照できない)。
- それ以外の先頭メッセージはすべて `Session::recv_request` へ渡し、結果で分ける。
  (1) 扱わない 3 種類は library が `REQUEST_ERROR` (NOT_SUPPORTED、control GOAWAY を送信済みなら
  GOING_AWAY) を `SendOnStream` として積むため、peer へ応答を返してセッションを維持する。
  (2) 7 種類以外 (decode できる型) は library が `PROTOCOL_VIOLATION` で closing にし、
  `CloseSession` を積む。
- 先頭メッセージと送信半を main ループへ運ぶ `BidiMessage::PeerRequest` は `request_id: u64` を
  持ち、request_id を持たないメッセージ ((2) の応答専用型など) を運べない。また `pump_once` の
  同 arm は `ClientEvent::Request` を無条件にアプリへ積むため、この経路を (1)(2) に使い回すと
  「アプリへは要求として渡さない」が成立しない。`BidiMessage` に「アプリへ渡さない先頭
  メッセージ」用の variant (`PeerRequestRejected { message, send, stop_tx }` など) を追加し、
  `pump_once` に専用の arm を足す。
- `pump_once` の追加 arm は次を行う。
  - `Session::recv_request(message)` を呼ぶ。
  - `Ok` のときは、返ってきた応答を `drain_events` が書けるよう、**`drain_events` の前に**
    送信半を `bidi_sends` へ、`stop_tx` を `bidi_stop_txs` へ登録する (台帳に無いと
    `SendOnStream` が `Skip` され応答が捨てられる)。応答には Request ID が要るため、
    `ControlMessage::Unsupported` の `request_id` を使う。
    `stop_tx` を登録しないと accept task の reader が終了して受信半が drop され、peer へ
    STOP_SENDING が届く。
  - `Err(RecvRequestError::Session(err))` のときは、`err` を `TransportError::Internal` に畳まず
    `drain_events` の `CloseSession` 処理へ届くようにする (`TransportError::Internal` に写すと
    `?` で `drain_events` を飛ばし、終了コードが peer に届かない)。扱わない 3 種類でも
    parity 違反・Request ID 重複のときは `INVALID_REQUEST_ID` で closing になるため、この扱いは
    (1)(2) の両方に必要である。
  - アプリへは `ClientEvent::Request` を積まない (積むと `Session::send_request_error` が未登録の
    Request ID を拒否し、example が致命終了する)。
- 検証は `examples/tokio-moq/src/peer_harness.rs` (0242) を拡張して行う。追加するのは次の 2 つで
  ある。
  - peer が開いた request stream に届く応答を読む API (`send_request_start` が保持している受信半
    を `ControlStream` として読む)。`REQUEST_ERROR` (NOT_SUPPORTED) の観測に使う。
  - 終了コードの観測は `MoqtClient::next_event` の
    `ClientEvent::Session(SessionEvent::CloseSession(err))` の `err.code` で行う。peer 側で
    CONNECTION_CLOSE を観測する経路は追加しない (必要になった時点で別 issue にする)。
- テストの入力は decode できる型に限る。完了条件 1 の入力には §6.3 の 7 種類でない
  decode 可能な型 (REQUEST_OK / SUBSCRIBE_OK / GOAWAY など) を使う。未知の type ID は decode
  失敗になり `BidiMessage::PeerRequestError` の既存経路 (0222) を通るため、本 issue の経路を
  固定できない。完了条件 2 の入力には `ControlMessage::Unsupported`
  (`type_id` は 3 種類、`request_id` は peer 役割の奇数、`body` は Request ID の varint) を使う。
  `body` は 0241 (未対応 request の本体を読み進める) が実装されても妥当な形にする。3 種類の
  共通構造は Request ID → Track Namespace Prefix → Number of Parameters → Parameters である
  ため、Request ID の varint に続けて Track Namespace Prefix (空なら count 0) と
  Number of Parameters 0 を書く (NOT_SUPPORTED の判断は `body` の内容に依存しないため、検証の
  目的は変わらない)。
- doc を実装に合わせて直す。対象は `Session::recv_request` (3 種類の扱い)、
  `examples/tokio-moq/src/moqt_client.rs` の `request_id_of` (§6.3 を 4 種類と誤引用)、
  `src/session/types.rs` の `RequestKind` (同じ誤引用) である。

### 実装時に変更した点

- `BidiMessage::PeerRequestRejected` は `stop_tx` を持たない。拒否した request は Session が状態を
  持たないため `SessionEvent::StopSendingRequestStream` が来ず、STOP_SENDING の依頼チャネルは
  不要である。受信半は「終端まで読む」ことで保持する (drop すると peer へ STOP_SENDING が届く)。
- 拒否したストリームの終端は `BidiMessage::RejectedRequestStreamClosed` で main ループへ渡し、
  `Session::recv_request_stream_closed` で拒否した request の記録を回収する。
- 詳細と理由は解決方法を参照する。

## 完了条件

- peer が decode できる 7 種類以外のメッセージを request stream の先頭に送ったとき、session が
  `PROTOCOL_VIOLATION` で closing になり、`ClientEvent::Session(SessionEvent::CloseSession(err))`
  の `err.code` が `PROTOCOL_VIOLATION` になること (example の I/O 層が終了コード付きの
  セッション終了を送る。peer 側で CONNECTION_CLOSE を観測することは本 issue の完了条件に
  含めない)。
- peer が扱わない 3 種類 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) を先頭に
  送ったとき、peer が同じ request stream で `REQUEST_ERROR` (NOT_SUPPORTED、control GOAWAY を
  送信済みなら GOING_AWAY) を受け取り、セッションが維持されること。アプリへ `ClientEvent::Request`
  が渡らないこと。
- `Session::recv_request` / `request_id_of` / `RequestKind` の doc が実装と一致していること。
- 上記がテストで固定されていること。モック / スタブを使わないこと。

## 解決方法

- `examples/tokio-moq/src/moqt_client.rs` の `spawn_peer_bidi_accept_task` は、先頭メッセージを
  `request_id_of` で「アプリへ渡す 4 種類 (SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS) か、
  それ以外か」に分けるようにした。§6.3 (Session initialization) の 7 種類判定は
  `Session::recv_request` が持つため example 側では複製しない。
- `BidiMessage::PeerRequestRejected { message, send }` を追加し、4 種類以外の先頭メッセージは
  アプリへ渡さず Session に判定させる。`pump_once` の新しい arm は `Session::recv_request` を
  呼び、`Ok` のときは応答を書けるよう `drain_events` より先に送信半を `bidi_sends` へ登録する
  (台帳に無いと `SendOnStream` が `Skip` され、応答が捨てられて peer から見て無応答になる)。
  `Err(RecvRequestError::Session)` のときは `TransportError::Internal` に畳まず `drain_events` に
  `CloseSession` を処理させ、終了コード付きのセッション終了を peer へ送る。
- 設計方針では拒否した request の `stop_tx` を `bidi_stop_txs` へ登録する予定だったが、拒否した
  request は Session の request table に登録されないため `SessionEvent::StopSendingRequestStream` が
  来ない。`stop_tx` は持たせず、代わりに `read_rejected_request_stream` が受信半を保持したまま
  終端まで読み、`BidiMessage::RejectedRequestStreamClosed { request_id, end }` で終端を main ループへ
  伝える。main ループは `Session::recv_request_stream_closed` を呼んで拒否した request の記録を
  回収し、`bidi_sends` からも外す (通知しないと記録がセッション生存中に残り続ける)。また通常の
  `BidiMessage::Response` の経路に流すと `Session::recv_stream_message` が 2 通目の "First" 型を
  PROTOCOL_VIOLATION で閉じるため、拒否したストリーム専用の読み捨て reader にした。この回収は
  公開 API からは観測できないため、テストでは固定できないことをコードに明記した。
- 既存の `BidiMessage::PeerRequest` (4 種類) の arm も、`Err(RecvRequestError::Session)` を
  `TransportError::Internal` に畳まず `drain_events` に委ねるようにした。parity 違反・Request ID
  重複では 4 種類でも同じ経路で closing になるため、終了コードを peer へ届けるには両方の扱いが
  必要である。
- doc を実装に合わせた。`Session::recv_request` (未対応 3 種類は `REQUEST_ERROR`
  (NOT_SUPPORTED / GOING_AWAY) を積んで維持し、§1.6 (Modularity) を引用。本体を解析しないため
  `accept_peer_request` を通らないこと)、`examples/tokio-moq/src/moqt_client.rs` の `request_id_of`
  (§6.3 は 7 種類であり、アプリへ渡すのは 4 種類)、`src/session/types.rs` の `RequestKind`
  (同じ誤引用) の 3 件である。
- テストは `examples/tokio-moq/src/peer_harness.rs` を拡張して 3 本追加した (計 5 本)。peer が
  開いた request stream に届く応答を読む `recv_response` と、送信方向を FIN で閉じる
  `finish_request_stream` を追加し、(1) REQUEST_OK (§6.3 の 7 種類に含まれない decode 可能な型) を
  先頭に送ると `CloseSession` の `err.code` が `PROTOCOL_VIOLATION` になること、(2)
  PUBLISH_NAMESPACE (`ControlMessage::Unsupported`、body は §9.14 (PUBLISH_NAMESPACE) /
  §8.7 (Track Namespace Structure) の構造) を先頭に送ると peer が `REQUEST_ERROR`
  (NOT_SUPPORTED) を受け取り、アプリへ要求が渡らず、拒否のあとも通常の request が受理されること、
  (3) parity 違反の SUBSCRIBE で `INVALID_REQUEST_ID` の `CloseSession` が届くことを固定した。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。テストは
  20 回以上の連続実行でフレークが無いこと、変異テストで主要な欠陥 (accept task の旧挙動、送信半の
  未登録、`TransportError::Internal` への畳み込み、アプリへの配送、登録順の反転など) を検出できる
  ことを確認した。回収処理だけは公開 API から観測できずテストで固定できない (コードに明記)。
