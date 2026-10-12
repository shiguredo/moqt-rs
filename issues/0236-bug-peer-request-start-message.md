# request stream の先頭メッセージを種別に応じて拒否またはセッション終了にする

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
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
