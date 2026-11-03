# WebTransport over HTTP/3 の draft 17 交渉に対応する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h3-draft-17-settings-version-negotiation
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-webtrans-http3-17 §3.1、§7.1、§9.2](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。

## 現状

`crates/tokio-moq/src/webtransport_h3.rs` は `ClientConfig::enable_webtransport` から依存の Settings を使う。
固定中の `shiguredo_http3 2026.1.0-canary.13` の `src/settings.rs` の `ID_WT_ENABLED` は `0x2c7cf000`、`src/webtransport/connect/draft.rs` の `DraftVersion` は Draft15 までである。
draft 17 の §9.2 は `0x20b8933b` を割り当てる。

## 設計方針

新しい SETTINGS と §7.1 の版選択に対応する依存版へ更新し、adapter の有効化 API と検証を追従させる。
互換性を破る変更のある draft が別 codepoint を使うことと、同じ codepoint を共有する版があることを区別する。
現行文書の codepoint と旧 draft codepoint が併存するときの優先規則、client からの SETTINGS の扱いを同節に合わせる。

## 完了条件

- 新 ID だけの相手との交渉を検証し、旧 ID だけの peer を新 wire 版として誤認しないこと。
- 複数版を広告する場合は共通版の選択が §7.1 と一致し、異なる版の仕様を混用しないこと。
- 固定依存・コメント・接続テストの対象版が一致すること。RESET_STREAM_AT の既存制約は別途追跡すること。

## 差分の確認範囲

- §3.2 の前接続 SETTINGS のキャッシュ破棄は 0408、§4.3 の WT_STREAM の位置と終了コードは 0409、§4.4 の Reliable Size は 0410 で追跡する。
- §5.6.2 / §5.6.4 の初期値を基準にする厳密増加は、固定中の依存の WebTransport Session::process_capsule が初期値を remote_limits に設定したうえで <= を拒否しており対応済みである。
- §4.6 の応答前 stream / datagram の拒否・drop と再送の説明は確認済み。adapter は 2xx 応答を待ってから session を返すため、optimistic な送信や自動再送を追加しない。
- §4.4 / §5 の intermediary のヘッダ長調整と上限転送は、この endpoint adapter の責務外である。
- §2.2 の HTTPS URI、重複文の削除、§8 の flow control 無効時の同居 HTTP との deadlock の説明、参照・IANA の変更も確認済み。現行 adapter は接続を専用に使い、他の HTTP 要求と多重化しない。

## 依存・関連

- 関連: [0408](0408-wt-h3-draft-17-resumption-current-settings.md)
- 関連: [0409](0409-wt-h3-draft-17-wt-stream-placement-error.md)
- 関連: [0410](0410-wt-h3-draft-17-reset-reliable-application-offset.md)
