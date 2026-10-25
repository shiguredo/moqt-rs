# 複数 session 併走による request 単位の GOAWAY 移行の設計

## この文書について

request stream GOAWAY の非空 New Session URI
移行を実装するための設計である。設計 issue の成果物として、
examples (`moq-pub` / `moq-sub`) の単一 session 前提を複数 session 併走へ拡張する方法を決定する。

## 1. 前提と現状

### 1.1 仕様

draft-ietf-moq-transport-22 §9.2 (GOAWAY) は、request stream 上の GOAWAY を受けた endpoint に、
"the endpoint SHOULD re-issue that specific request on a session at the specified URI (or the current session if
no URI is provided), and close the old request stream using the appropriate mechanism (e.g. FIN, stream reset, or
PUBLISH_DONE)" を SHOULD で要求する。§6.6.1 (Graceful Session Migration) は新しい session の確立と移行の
期限を、§7.6.1 (Graceful Publisher Relay Switchover) は publisher 側の移行を定める。

### 1.2 現在の examples の構造

- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` の main ループは、1 つの
  `MoqtClient` と 1 つの `DataPlaneHandle` を所有する。
- control GOAWAY の移行は session 全体の置換である。`migrate_publisher_session` は新 session を確立して
  `MigratedSession` を返し、main ループの `migration_outcome` 分岐が `client` / `data_plane` を差し替える。
  `spawn_old_session_close` は旧 session の PUBLISH を PUBLISH_DONE で終端して NO_ERROR で閉じる。
- request stream GOAWAY の現状は publisher と subscriber で異なる。
  - moq-pub の `goaway_action` は、空 URI なら現在の session で当該 PUBLISH を再発行し、非空 URI は
    `IgnoreRequestScope` として警告して移行しない。
  - moq-sub は `migration_target` と `same_session_url` を使い、移行先 URL が現在の URL と同じなら現在の
    session で当該 request (`goaway_request_kind` / `reissue_request`) を再発行する。別 URL のときは移行しない。
- moq-sub は Track Alias の対応表 `track_map` (alias → TrackKind) と `media_fetch_targets` (fetch request ID →
  TrackKind) を持ち、stream task は 1 つの `DataPlaneHandle` とこれらの表を共有する。moq-pub は両表を持たず、
  固定 alias (`VIDEO_TRACK_ALIAS` / `AUDIO_TRACK_ALIAS`) と `PublisherSession` の request ID、`data_plane` を
  共有する。
- `examples/tokio-moq/src/peer_harness.rs` の `goaway_migration_switches_to_the_new_session` は 2 つの
  `TestPeerServer` / `TestPeer` / `MoqtClient` を同時に保持して control GOAWAY 移行を検証済みである。ただし
  `peer_harness` は `#[cfg(test)]` モジュールであり、moq-pub / moq-sub のテストからは参照できない
  (test-support feature 化は別途計画中)。2 session を同時に駆動する補助 API は無い。

## 2. 制約

- CODEBASE.md のとおり、`Session` は 1 本の MOQT Transport Session に閉じた endpoint-local の protocol state
  machine であり、Sans I/O / no_std を守る。本設計ではライブラリの `Session` API を複数 session 対応に拡張しない。
  session の所有とルーティングは examples と `examples/tokio-moq` の範囲に閉じる。
- relay 機能は追加しない。モック・スタブは使わず、失敗系も実 peer の REQUEST_ERROR / 無応答 / 切断で作る。
- 設計の反映先の issue は、reopened の理由を追記してから実装対象へ戻す。

## 3. 設計

### 3.1 session 管理

`MoqtClient` の送信 API / `tick_now` / `next_event` は `&mut self` を要求するため、main ループが複数の
`MoqtClient` を保持して `next_event` を回す方式は成立しない。session ごとに専用タスクが `MoqtClient` を所有し、
main ループはチャネルでコマンドとイベントをやり取りする。

```text
SessionHandle {
    id: SessionId,                    // example 内で一意な連番
    commands: mpsc::Sender<SessionCommand>,
    events: mpsc::Sender<(SessionId, ClientEvent)>,      // main ループが持つ 1 本の受信チャネルへ送る
    alias_map: Arc<StdMutex<HashMap<u64, TrackKind>>>,   // この session の Track Alias 対応表
    request_map: Arc<StdMutex<HashMap<u64, TrackKind>>>, // この session の request ID 対応表
    url: ServerUrl,
}

SessionCommand {
    Send(Box<dyn FnOnce(&mut MoqtClient) -> ...>), // 実際は種別ごとの enum にする
    Tick,
    Shutdown,
}
```

- session task は `MoqtClient` を所有し、`commands` を待つ `select!` と `next_event` のポーリングを交互に行う。
  イベントは `(SessionId, ClientEvent)` として main ループが持つ 1 本のイベントチャネルへ送る。
- main ループは 1 本のイベントチャネルを `select!` で待ち、`SessionId` から `SessionHandle` を解決する。
  複数 session のイベントは到着順に処理される。
- `tick` は main ループの周期処理で全 `SessionHandle` に `SessionCommand::Tick` を送る。data stream timeout /
  delivery timeout / GOAWAY deadline は session 単位で進む。
- session の終了 (`CloseSession`) はイベントとして通知し、main ループが handle を `Draining` にして掃除する。
  task の JoinHandle は handle が保持し、破棄時に abort する。
- 送信 API は `SessionCommand` の enum で表現する。汎用のクロージャを送ると `Send` 制約とエラー型の扱いが
  複雑になるため、`Subscribe` / `Fetch` / `Publish` / `SendObject` / `SendRequestError` など必要な操作だけを
  enum variant にする。戻り値は oneshot チャネルで返す。

### 3.2 request と session の対応

アプリの識別子は request ID 単独ではなく `(SessionId, request_id)` の組とする。

```text
RequestOwner {
    session: SessionId,
    request_id: u64,     // その session 内の Request ID
    kind: RequestKind,   // Subscribe / Fetch / Publish
    track: TrackKind,    // Video / Audio / Catalog / Other
}
```

- 論理 track (`TrackKind`) ごとに `RequestOwner` を 1 つ保持し、移行時に差し替える。
- `SessionEvent` の request ID は handle の `session.id` と組にしてからアプリの対応表を引く。request ID が
  session 間で衝突しても取り違えない。
- 送信 API は `RequestOwner` から `SessionHandle` を解決して `SessionCommand` を送る。

### 3.3 data plane のルーティング

- subscriber 側 (moq-sub): 受信 data stream の処理タスクは、起動時に
  `(SessionId, DataPlaneHandle, Arc<Mutex<HashMap<u64, TrackKind>>>)` を受け取る。`SUBGROUP_HEADER` の Track
  Alias はその session の alias 表で解決する。alias は session ごとに独立しているためグローバルな対応表は
  持たない。`media_fetch_targets` も session ごとの表に分ける。
- publisher 側 (moq-pub): 固定 alias と `PublisherSession` の request ID を session ごとに分離する。
  旧 session の Object 送信と新 session の PUBLISH を並行させるため、video / audio / catalog の送信経路は
  `RequestOwner` から session を解決して `SessionCommand::SendObject` を送る。
- FETCH 応答 / fill fetch stream は `(SessionId, request_id)` をキーにした
  `fetch_owner: HashMap<(SessionId, u64), TrackKind>` で解決する。`OpenFillFetchStream` イベントは
  その session の `DataPlaneHandle` で処理し、開いた stream を同じ session に登録する。
- datagram は session ごとの受信ループから `(SessionId, track_alias)` で alias 表を引く。
- peer request は session ごとに応答する。応答の `send_request_error` / `send_request_ok` は受信した session の
  `SessionCommand` に対して送る。
- `DataPlaneHandle` は session ごとに 1 つであり、ライブラリ API の変更は不要である。変更点は
  「どの `DataPlaneHandle` を使うか」を stream task と main ループが明示的に受け取ることに限る。

### 3.4 移行状態機械

移行対象 request ごとに次の状態を持つ。

```text
Idle
  -> Received { target_url, deadline }        // §9.2 受信
  -> Establishing { target_url, deadline }    // 新 session 確立中 (URL ごとに 1 回)
  -> Reissuing { new_session, new_request_id }// 新 session で再発行済み
  -> AwaitingOk { new_session, new_request_id }// 応答待ち
  -> Completed { old_session, new_session }   // 受理後に旧 request を終端
  -> Failed { reason }                        // 期限切れ / 拒否 / 新 session 断
```

- `Establishing` は control GOAWAY 移行の接続確立・認可トークン引き継ぎ・指数バックオフ・期限の既存実装
  (`establish_publisher_session_with_retry` / `migration_target` / `MigrationDeadline`) を共通化して使う。
  control GOAWAY と異なり旧 session を閉じず、`SessionHandle` を追加するだけにする。
- `Reissuing` では request 種別ごとの再発行 API を新しい handle に対して送る。
- `AwaitingOk` で受理 (`SUBSCRIBE_OK` / `FETCH_OK` / `PUBLISH_OK`) を受けたら、旧 request を種別に応じて
  終端する。SUBSCRIBE / FETCH は bidi request stream を FIN または RESET_STREAM、PUBLISH は
  PUBLISH_DONE (GOING_AWAY) で終端する (§9.2 の "appropriate mechanism")。
- 旧 request の終端後、`RequestOwner` を新 session の組へ差し替え、旧 session は他の request が残る限り
  維持する。残りの request と data plane が無くなったら旧 session を close する。
- 期限切れは `Failed` とし、control GOAWAY 移行と同じ警告を出して main ループを終了する (アプリの判断)。
  request stream GOAWAY の Timeout は当該 request stream を GOING_AWAY で reset するだけで、session 自体は閉じない。
  「旧 session で継続」には倒さない理由は、peer が当該 request の受信を期限後に打ち切るためである。

### 3.5 同一 URL と複数 request の共有

- `migration_target` で解決した移行先 URL が現在の session の URL と同じ場合は、新 session を作らず現在の
  session で再発行する (moq-sub の `same_session_url` の既存挙動を保存する)。別 URL のときだけ新 session を
  確立する。
- 同一移行先 URL を指す複数 request は 1 つの新 session を共有する。moq-sub は catalog / video / audio の 3
  request を持ち、同じ GOAWAY URI が届くことがあるため、URL ごとにセッション確立を 1 回にまとめる。
  状態機械は URL 単位の `Establishing` と request 単位の `Reissuing` / `AwaitingOk` を分けて持つ。

### 3.6 重複と順序

- 論理 track ごとに「最後にアプリへ渡した Location」(`delivered_watermark: HashMap<TrackKind, Location>`) を
  保持する。移行中は新旧 session の両方から Object が届き得るため、受信 Object の Location が watermark
  以下ならアプリへ渡さず破棄する。watermark はアプリへ渡した時点で更新する。
- catalog の適用は `CatalogState` へ到着順に適用する既存経路を維持し、重複 Object は watermark で落とす。
  catalog の delta は Location 順に届くため watermark による重複排除で十分である。
- 映像・音声の decode order は論理 track 単位で維持する。移行直後は watermark より後の Object だけを復号へ
  渡すため、`VideoDecodeOrder` / `AudioDeliveryOrder` は session をまたいで共有する。

### 3.7 request 種別ごとの移行手順

| 種別 | 再発行 | 旧 request の終端 | 備考 |
| --- | --- | --- | --- |
| SUBSCRIBE | 新 session で同じ namespace / track / パラメータ (FILL_PARAMETERS を含む) を送る | bidi stream を FIN (未完了なら RESET_STREAM) | 新 session で `OpenFillFetchStream` が発行され得る。fill range は新 session の subscription で再計算する |
| FETCH | 新 session で同じ range / パラメータを送る | bidi stream を FIN / RESET_STREAM、data stream は終端通知 | 応答 data stream は新 session 側で `fetch_owner` に登録する |
| PUBLISH | 新 session で同じ track / properties を送る | PUBLISH_DONE (GOING_AWAY) | 新 session で PUBLISH_OK を受けるまで旧 session で Object を送り続ける。切替後は旧 session の incoming を停止する |

- 認可トークンは control GOAWAY 移行と同じ規則で引き継ぐ (同一 origin のときだけ引き継ぎ、別 origin では
  再取得する)。移行先 URL は `migration_target` で解決する。
- FILL_PARAMETERS 内のパラメータは subscription 状態として保持しない (§9.20.15) ため、再発行時に
  `RequestOwner` が保持する元の要求パラメータから再構成する。
- 旧 session で移行対象 request を破棄するときは、forget_subscription 時の STOP_SENDING の既存挙動と
  二重に STOP_SENDING を発行しないよう、Session 側の終端通知の順序を確認する。

### 3.8 失敗時

- 新 session の確立失敗・再発行の拒否 (REQUEST_ERROR)・新 session の切断は `Failed` とし、理由をログに出す。
- GOAWAY の Timeout は `GoawayReceived.timeout` を絶対時刻へ変換して保持し、`Establishing` / `AwaitingOk`
  の各段階で残り時間を確認する。期限を過ぎたら背景タスクを abort し、main ループを終了する。
- 旧 request stream の終端に失敗した場合はセッションを閉じずにログを出し、slot の掃除で回収する。
- 失敗系のテストは実 peer の REQUEST_ERROR・無応答・切断で作り、モックやスタブは使わない。

### 3.9 観測

- `SessionEvent::GoawayReceived { on_request_stream: Some(request_id), new_session_uri, timeout }` で
  移行対象を特定できる。example は `(SessionId, request_id)` を `RequestOwner` に解決してから移行を開始する。
- 移行の開始・確立・再発行・完了・失敗を `tracing` で記録する。control GOAWAY 移行と同じイベント名・
  レベルに揃える。
- 移行回数と失敗数を example のログで観測できるようにする。メトリクスライブラリの追加はしない。

### 3.10 テスト

- `peer_harness` は既に 2 つの `TestPeerServer` / `MoqtClient` を同時に保持する control GOAWAY 移行テストを
  持つため、2 session の同時駆動自体は可能である。ただし `#[cfg(test)]` のため moq-pub / moq-sub から参照
  できず、2 session を同時に駆動する補助 API は無い。テストは次のいずれかで実装する。
  - test-support feature の計画を先行して実装し、`peer_harness` を examples のテストから使う。
  - moq-pub / moq-sub の `#[cfg(test)]` 内に、`peer_harness` と同じ構成の 2 session 補助を置く。
  どちらを採るかは test-support feature の進捗で決める (実装タスクの依存として明記する)。
- 検証項目:
  1. 非空 URI の request stream GOAWAY で指定 URI の session が確立され、当該 request だけが再発行される
  2. 旧 session の他の request と data plane (Object 送受信) が継続する
  3. subscriber の同一 URL の非空 URI の request stream GOAWAY は現在の session で再発行される (既存挙動の保存)
  4. publisher の同一 URL の非空 URI の request stream GOAWAY は現在の session で PUBLISH が再発行される
     (新規実装)
  5. 同一移行先 URL を指す複数 request は 1 つの新 session を共有する
  6. 空 URI の request stream GOAWAY は従来どおり現在の session で再発行される
  7. 期限内に移行できない場合の警告と終了
  8. 移行中の重複 Object がアプリへ二重に渡らない
- テストは実 peer の REQUEST_ERROR / 無応答 / 切断で failure injection を作る。

## 4. 実装タスク分割

1. `examples/tokio-moq` に session task / `SessionHandle` / `SessionCommand` / イベント集約の共通基盤を追加する。
   対象: `examples/tokio-moq/src/` (新モジュール。`lib.rs` からの公開を検討する)。
2. 2 session を同時に駆動するテスト補助を用意する。test-support feature を先行するか、example 内の test 補助かを
   決めて実装する。対象: `examples/tokio-moq/src/peer_harness.rs` または test-support 実装。
3. moq-sub の main ループを `SessionHandle` ベースへ変更し、request 単位 GOAWAY の状態機械と data plane
   ルーティングを実装する。対象: `examples/moq-sub/src/pipeline.rs`。
4. moq-pub の main ループを `SessionHandle` ベースへ変更し、PUBLISH の request 単位移行と旧 session の
   PUBLISH_DONE を実装する。対象: `examples/moq-pub/src/pipeline.rs`。
5. 移行中の重複排除 (watermark) と catalog / decode order の共有状態を実装する。対象: 両 example。
6. docs (`docs/moqt.md` / `docs/missing-features.md`) を実装に合わせて更新する。
7. テスト (移行成功・同一 URL・失敗・重複排除) を追加する。

## 5. 実装時の受入基準

- 非空 New Session URI の request stream GOAWAY を受けたとき、移行先 URL が現在の URL と異なる場合は指定 URI の
  session が確立され、当該 request だけが新 session で再発行され、旧 request stream が FIN / RESET_STREAM /
  PUBLISH_DONE の適切な方法で閉じられること。
- 移行先 URL が現在の URL と同じ場合、subscriber は新 session を作らず現在の session で当該 request が再発行される
  こと (既存挙動の保存)。publisher は非空 URI の同一 URL でも、現在の session で PUBLISH が再発行されること
  (新規実装)。
- 同一移行先 URL を指す複数 request は 1 つの新 session を共有すること。
- 旧 session の他の request と data plane が継続すること (Object の送受信が途切れないこと)。
- 空 URI の request stream GOAWAY は従来どおり現在の session で再発行されること。
- 期限内に移行できない場合、control GOAWAY 移行と同じ警告と終了になること。
- 移行中に旧 session で in-flight だった Object がアプリへ二重に渡らないこと。
- `docs/moqt.md` / `docs/missing-features.md` の記述が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
