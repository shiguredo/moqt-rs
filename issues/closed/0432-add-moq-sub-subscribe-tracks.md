# moq-sub に SUBSCRIBE_TRACKS の送信と PUBLISH の受理を追加する

- Created: 2026-11-14
- Completed: 2026-11-14
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)
- Polished: {YYYY-MM-DD}

## 目的

moq-sub は catalog track を SUBSCRIBE して取得し、catalog が示す映像・音声 track を個別に
SUBSCRIBE する。draft-ietf-moq-transport-23 §3.6 (Subscribing to Tracks by Prefix) /
§9.17 (SUBSCRIBE_TRACKS) の namespace prefix 購読を送る手段が無く、publisher に namespace
配下の track を PUBLISH させる経路を example から確認できない。moqt-rs 側は
`Session::send_subscribe_tracks` と `MoqtClient::subscribe_tracks`、PUBLISH を受理する
`MoqtClient::send_request_ok`、`SessionEvent::PublishSkippedReceived` を実装済みであるため、
moq-sub に設定手段と受理経路を追加する。

## 現状

- `crates/moq-sub/src/cli.rs` の `Config` に SUBSCRIBE_TRACKS の設定が無く、`parse_from` も
  フラグを定義していない。
- `crates/moq-sub/src/pipeline.rs` の `run` は catalog track を
  `subscribe_track_with_parameters` で購読したあと、catalog から選んだ映像・音声 track を
  SUBSCRIBE する。namespace 単位の購読は送らない。
- 同じ `run` の `ClientEvent::Request` 分岐は、`request_rejection` で PUBLISH を
  `UNINTERESTED` (0x20)、それ以外を `NOT_SUPPORTED` (0x3) として拒否する。
- `SessionEvent::PublishSkippedReceived` と、SUBSCRIBE_TRACKS の結果として届く `PUBLISH` は
  処理していない。
- `crates/tokio-moq/src/moqt_client.rs` の `MoqtClient::subscribe_tracks` は REQUEST_OK /
  REQUEST_ERROR まで待って Request ID を返す。`IncomingRequest.message` の `Publish` から
  `parameters.subscribe_tracks_id()` と `track_alias` を読める。
- `MoqtClient::subscribe_tracks` は AUTHORIZATION_TOKEN を付与しない。moq-sub は接続時の
  SETUP に URL の C4M トークンを載せるため、メッセージ単位のトークン付与は本 issue の対象外と
  する。
- moq-pub は SUBSCRIBE_TRACKS を受理せず、e2e-tests の publisher から PUBLISH を返す経路が
  無い。moq-sub 単体の結合テストは peer の代わりを立てられないため、判定関数の単体テストで
  固定する。

## 設計方針

- `crates/moq-sub/src/cli.rs` の `Config` に `subscribe_tracks: bool` を追加し、CLI に
  `--subscribe-tracks` フラグを追加する。既定は無効とする。help は英語で、SUBSCRIBE_TRACKS を
  送り、結果の PUBLISH を受理して Object を再生に使わないことを説明する。
- 送信する prefix は `--namespace` で解決した Track Namespace とする。SUBSCRIBE_TRACKS の
  パラメータは空にする。
- 送信は catalog の購読と映像・音声の SUBSCRIBE が終わった後に行う。失敗 (REQUEST_ERROR /
  セッション終了 / タイムアウト) しても既存の再生を止めず、警告ログだけ残す。
- `ClientEvent::Request` で `ControlMessage::Publish` を受け、
  `parameters.subscribe_tracks_id()` が送信した Request ID と一致する場合だけ受理する。
  `MoqtClient::send_request_ok` で PUBLISH_OK を返し、PUBLISH が運ぶ `track_alias` を
  `track_map` に登録する。
- 受理した購読は再生・録画に使わない。`TrackKind` に `Discard` を追加し、subgroup stream と
  datagram は読み捨てる。catalog / 映像 / 音声の購読と Full Track Name が重複しても、既存の
  購読に影響を与えないよう受理して Object を捨てる。ただし PUBLISH の Track Alias が既存の
  購読の alias と衝突する場合は、data stream がどちらの購読のものか alias だけでは区別できない
  ため、`UNINTERESTED` で拒否する。
- `SessionEvent::PublishSkippedReceived` は記録のための info ログを出す。PUBLISH の
  REQUEST_ERROR は既存の警告経路で扱う。
- 受理した購読の PUBLISH_DONE は、購読の drain が終わるまで `publish_done_pending` に載せ、
  tick で alias を外す。`Discard` は映像・音声の終了判定に含めない。
- セッション移行 (control stream の GOAWAY) の後は、移行先で SUBSCRIBE_TRACKS を送り直す。
  request stream の GOAWAY は再発行しない。
- 停止時の STOP_SENDING 対象には含めない。PUBLISH 起点の購読はセッション終了で終わる。
- `crates/moq-sub/README.md` の CLI オプション表と `docs/missing-features.md` の moq-sub の
  peer request 拒否方針を、`--subscribe-tracks` を有効にしたときの PUBLISH 受理に合わせて
  更新する。

## 完了条件

- `--subscribe-tracks` を付けたときだけ、catalog / 映像 / 音声の購読確立後に namespace prefix の
  SUBSCRIBE_TRACKS が送られること。
- 送信した Request ID に一致する SUBSCRIBE_TRACKS_ID を持つ PUBLISH が PUBLISH_OK で受理され、
  その alias の Object が再生・録画に使われずに読み捨てられること。
- 一致しない PUBLISH と、`--subscribe-tracks` 無効時の PUBLISH は従来どおり拒否されること。
- 既存の購読と同じ Track Alias を持つ PUBLISH は `UNINTERESTED` で拒否され、既存の購読の
  alias 表が書き換わらないこと。
- PUBLISH_SKIPPED / REQUEST_ERROR / PUBLISH_DONE / control stream GOAWAY の移行で、既存の
  catalog / 映像 / 音声の受信と状態が壊れないこと。
- `Config` の既定値と `--subscribe-tracks` の解釈が CLI のテストで固定されていること。
- PUBLISH の受理判定が単体テストで固定されていること (一致 / 不一致 / Request ID 無し /
  パラメータ無し)。
- `crates/moq-sub/README.md` と `docs/missing-features.md` が実装に合っていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

- `crates/moq-sub/src/cli.rs` の `Config` に `subscribe_tracks: bool` を追加し、
  `--subscribe-tracks` フラグで有効にできるようにした。既定は無効である。
- `crates/moq-sub/src/pipeline.rs` の `run` で、catalog / 映像 / 音声の購読を確立したあとに
  `--namespace` の prefix で `MoqtClient::subscribe_tracks` を送る。失敗しても警告ログだけを
  出して既存の再生を続ける。
- `ClientEvent::Request` の PUBLISH を `SUBSCRIBE_TRACKS_ID` と送信した Request ID の一致で
  判定し、一致するものだけ `MoqtClient::send_request_ok` で受理する。受理した alias は
  `TrackKind::Discard` として `track_map` に登録し、subgroup stream と datagram の Object は
  再生・録画に使わず読み捨てる。
- 既に SUBSCRIBE している Full Track Name かどうかを `is_already_subscribed_publish` で判定し、
  重複する PUBLISH も Object を捨てる。既存の alias と衝突する PUBLISH は data stream を
  alias だけで区別できないため `UNINTERESTED` で拒否する。
- `PUBLISH_SKIPPED` は info ログ、SUBSCRIBE_TRACKS への `REQUEST_ERROR` は既存の警告経路で
  扱い、対応する Request ID を受理判定から外す。受理した購読の `PUBLISH_DONE` は
  `publish_done_pending` の drain 待ちに載せ、request stream の reset も含めて Session の回収が
  済んだ時点で alias を外す (`plan_for_migration` は Discard を移行計画に載せない)。
- control stream の GOAWAY による session 移行のあとは、移行先で SUBSCRIBE_TRACKS を送り直す。
  request stream 単位の GOAWAY では PUBLISH 起点の購読を再発行しない。
- テストを追加した。`Config` の既定値と `--subscribe-tracks` の解釈、SUBSCRIBE_TRACKS_ID の
  一致判定 (一致 / 不一致 / Request ID 無し / パラメータ無し)、重複判定、移行計画からの除外、
  プロセス実行時の `--help` 表示を固定している。
- `crates/moq-sub/README.md` に `--subscribe-tracks` の説明と動作を追加し、
  `docs/missing-features.md` の peer request 拒否方針を更新した。
- moq-pub が publisher 側の SUBSCRIBE_TRACKS に未対応で、relay に依存しない publisher 起点の
  PUBLISH 経路を example 間で固定できないため、e2e-tests への専用シナリオは追加していない。
  relay が SUBSCRIBE_TRACKS を受理する環境では `--subscribe-tracks` で確認できる。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ることを確認した。
