# Malformed Track 検出時に bidi request stream を cancel する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-malformed-track-bidi-cancel
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の MUST を満たす。Malformed Track を検出した subscriber が、対応する subscription の bidi request stream を §6.4.2.3 (Request Cancellation and Rejection) の cancel 手順で打ち切る。

## 現状

`src/session/data.rs` の `terminate_malformed_track` は `ResetDataStream` と `RequestTerminated { reason: MalformedTrack }` を発行するが、`ResetRequestStream` / `StopSendingRequestStream` を発行しない。`src/session/types.rs` の `TerminationReason::MalformedTrack` doc は cancel をアプリ責務としている。

§6.4.2.3 の cancel は両方向を対象とする。

> "Implementations cancel a request by abruptly terminating any directions of the stream that are still open, using RESET_STREAM for a direction they are sending and STOP_SENDING for a direction they are receiving. An endpoint that has already sent a FIN on its sending direction
> and subsequently wishes to cancel sends STOP_SENDING on the receiving direction."

subscription の cancel は §3.1 (Subscriptions) のとおり subscriber が STOP_SENDING を送る。`StopSendingRequestStream` は `src/` から一度も発行されておらず、`ResetRequestStream` を発行するだけでは受信方向が打ち切られない。

根拠 (draft-ietf-moq-transport-21 §12.1):

> "When a subscriber detects a Malformed Track, it MUST cancel any corresponding subscription or fetches for that Track from that publisher (see Section 6.4.2.3), and SHOULD deliver an error to the application."

## 設計方針

- `terminate_malformed_track` が新規に subscription を `Terminated` へ遷移させる経路で、対象 request の bidi request stream の cancel 指示を発行する。
  - `StopSendingRequestStream { request_id, error_code: STREAM_MALFORMED_TRACK }` (受信方向、§3.1 / §6.4.2.3)
  - `ResetRequestStream { request_id, error_code: STREAM_MALFORMED_TRACK }` (送信方向、§6.4.2.3)。送信方向が既に FIN 済みの場合は I/O 層が無視する
  - 発行順は受信を止める `StopSendingRequestStream` を先にし、続けて `ResetRequestStream` を発行する
- エラーコードは §12.5 (Stream Reset Error Codes) の `STREAM_MALFORMED_TRACK` (0x12) を両イベントに使う (§12.5: "The application SHOULD use a relevant error code when resetting or sending STOP_SENDING on any stream.")。
- 既に `Terminated` の経路では cancel イベントを発行しない。キャンセル由来 `Terminated` の早期 return と、PUBLISH_DONE 受信済み `Terminated` の `ResetDataStream` のみの経路 (既存テスト `malformed_after_publish_done_sends_reset_without_request_terminated` が固定) は変更しない。
- subgroup header / subgroup object / datagram のどの検出経路でも、対象 request が確定していれば同じ cancel を発行する (datagram は `stream_id` が `None` でも request_id は確定している)。
- fetch は対象外とする。`terminate_malformed_track` の現行呼び出しは subscription 由来のみで、FETCH の Malformed Track 検出自体が未実装である。
- doc を新挙動に合わせて更新する。
  - `src/session/types.rs` の `StopSendingRequestStream` doc「Session の自動発火経路は現在なく、受信方向の cancel は I/O 層主導で行う」を、本経路で Session が発行する記述に更新する
  - `TerminationReason::MalformedTrack` doc「アプリケーションは §6.4.2.3 に従い当該 bidi request stream を RESET_STREAM / STOP_SENDING で閉じる」を、Session が cancel イベントを発行する契約に更新する
  - `src/session/data.rs` の `terminate_malformed_track` doc と `TerminationReason::MalformedTrack` doc の引用 `(see Section 3.3.3)` は誤り。§12.1 の原文は `(see Section 6.4.2.3)` であり、本 issue で修正する (0038 の一括修正対象からは本件を除く)
- 依存: open issue 0042 は `StopSendingRequestStream` を「`src/` から一度も発行されない」として削除候補にしている。本 issue の実装後は使用されるため、0042 を実装する際は本 issue を先に取り込むか、0042 の該当項目を除外する前提とする。
- `CHANGES.md` の `## develop` に `[FIX]` として記載する。

## 完了条件

- 新規に Malformed Track を検出して subscription を `Terminated` にする経路で、`StopSendingRequestStream` と `ResetRequestStream` がどちらも `STREAM_MALFORMED_TRACK` (0x12) で発行されること
- subgroup header / subgroup object / datagram の各検出経路で cancel が発行されること
- 既に `Terminated` の経路 (キャンセル由来、PUBLISH_DONE 受信済み) では cancel イベントが発行されないこと
- 既存の `ResetDataStream` と `RequestTerminated { reason: MalformedTrack }` の挙動が維持されること
- `StopSendingRequestStream` / `TerminationReason::MalformedTrack` / `terminate_malformed_track` の doc が新挙動に追随し、`(see Section 3.3.3)` の誤引用が §6.4.2.3 に修正されていること
- 回帰テストが `tests/test_session/` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` として記載されていること
