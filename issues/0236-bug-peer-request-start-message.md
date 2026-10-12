# request stream の先頭が request でないメッセージを破棄せずセッションを閉じる

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-peer-request-start-message
- Polished: {YYYY-MM-DD}

## 目的

peer が開始した request stream の先頭メッセージの扱いが、draft-ietf-moq-transport-22
§6.3 (Session initialization) と合っていない。同節は先頭メッセージを 7 種類
(SUBSCRIBE / PUBLISH / FETCH / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / TRACK_STATUS /
SUBSCRIBE_TRACKS) に限り、それ以外は PROTOCOL_VIOLATION でセッションを閉じる。また
本実装が扱わない 3 種類 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) は
`REQUEST_ERROR` (NOT_SUPPORTED) で拒否してセッションを維持する。現状はどちらも警告ログを
出してメッセージを捨てるだけで、セッションの状態が変わらない。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `spawn_peer_bidi_accept_task` は、peer が開始した
  request stream の先頭メッセージを `request_id_of` で判定し、`None` の場合は `tracing::warn!` を
  出して return する。main ループへは伝えないため、session は Established のまま残る。
- `request_id_of` は `ControlMessage` の variant のうち 4 種類 (SUBSCRIBE / PUBLISH / FETCH /
  TRACK_STATUS) しか扱わない。本実装が扱わない 3 種類は `ControlMessage::Unsupported`
  (デコード済みの request 構造体を持たず、`type_id` / `request_id` と生の `body` を保持する)
  として届くため、§6.3 の 7 種類に含まれるにもかかわらず同じ warn 破棄の経路に落ちている。
- decode 失敗は `BidiMessage::PeerRequestError` で main ループへ伝えてセッション終了に
  つなげている (0222 で対応)。先頭メッセージの種別判定だけが同じ扱いから漏れている。
- library の `Session::recv_request` は 7 種類以外を PROTOCOL_VIOLATION として closing に遷移
  させ、扱わない 3 種類は `REQUEST_ERROR` (NOT_SUPPORTED) を返してセッションを維持する。
  example はこれを呼ばずに破棄している。

## 設計方針

- 先頭メッセージの種別で 3 つに分ける。(1) 本実装が扱う 4 種類は従来どおり
  `BidiMessage::PeerRequest` として main ループへ渡す。(2) 扱わない 3 種類は
  `Session::recv_request` に渡して library に NOT_SUPPORTED (control GOAWAY を送信済みなら GOING_AWAY) を返させる。(3) 7 種類以外は
  同じく `Session::recv_request` に渡し、PROTOCOL_VIOLATION でセッションを閉じさせる。
- (2) では peer 起点 stream の送信半を `bidi_sends` の台帳へ新たに登録し ((1) と同じ登録経路を
  使い)、library が積んだ
  `SendOnStream` (`REQUEST_ERROR` など) を I/O 層が書けるようにする。台帳に送信半が無いと
  応答が破棄され、peer から見て無応答になる。アプリへは要求として渡さず、
  `send_request_error` を呼ばせない (二重応答を避ける)。(3) は `CloseSession` だけを積むため
  台帳は不要である。
- (3) で `Session::recv_request` がエラーを返した場合は、その `SessionError` を
  `Session::close` と同じ扱いで I/O 層まで通し、`drain_events` の `CloseSession` 処理で
  終了コード付きのセッション終了を送ってから example が終了するようにする。
  `TransportError::Internal` に写して伝播させると終了コードが peer に届かない。
- `Session::recv_request` の doc は「扱わない 3 種類は PROTOCOL_VIOLATION」と実装に反する
  記述になっているため、本 issue の中で実装に合わせて直す。あわせて
  `examples/tokio-moq/src/moqt_client.rs` の `request_id_of` の doc が §6.3 を 4 種類と
  誤引用している点も直す (同節は 7 種類、本実装が扱うのは 4 種類)。
- 判定は `request_id_of` ではなく「§6.3 の 7 種類か」で行う。`ControlMessage::Unsupported` は
  request 種別を持たないため、Request ID の取り出しと種別判定を分ける。
- アプリへ渡さないメッセージへ応答を送らないことで、同じ Request ID への二重応答
  (`send_request_error` の未登録 id 拒否) を避ける。
- 検証には peer を模擬する in-process 試験基盤 (0242) を使う。

## 完了条件

- peer が 7 種類以外のメッセージを request stream の先頭に送ったとき、session が
  PROTOCOL_VIOLATION で closing になり、example が終了コード付きで終了すること。
- peer が扱わない 3 種類 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) を
  先頭に送ったとき、`REQUEST_ERROR` (NOT_SUPPORTED、control GOAWAY を送信済みなら GOING_AWAY) が peer へ
  送られ、セッションが維持される
  こと。アプリへ要求が渡らず、二重応答にならないこと。
- `Session::recv_request` と `request_id_of` の doc が実装と一致していること。
- 上記がテストで固定されていること。モック / スタブを使わないこと。
