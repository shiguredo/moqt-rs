# WebTransport over HTTP/3 の draft 17 交渉に対応する

- Created: 2026-11-03
- Completed: 2026-11-08
- Branch: develop
- Polished: 2026-11-08

## 目的

[draft-ietf-webtrans-http3-17 §3.1、§7.1、§9.2](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従し、WebTransport over HTTP/3 の `SETTINGS_WT_ENABLED` を新しい codepoint `0x20b8933b` で交渉する。

対象は SETTINGS の codepoint と版選択である。データストリームの規則は §4.3 を 0409、§4.4 の Reliable Size を 0410 で追跡する。`DraftVersion::Draft17` は SETTINGS の codepoint を識別する値であり、draft-17 の全規定への準拠を意味しない。

## 現状

`crates/tokio-moq/Cargo.toml`、`crates/moq-pub/Cargo.toml`、`crates/moq-sub/Cargo.toml` は `shiguredo_http3 2026.1.0-canary.13` を固定している。canary.13 の `src/settings.rs` の `ID_WT_ENABLED` は `0x2c7cf000` で、`src/webtransport/connect/draft.rs` の `DraftVersion` は Draft15 までであり、新 codepoint を送れない。
draft 17 の §9.2 は `0x20b8933b` を割り当てる。

実際に codepoint を選んでいるのは `crates/tokio-moq/src/lib.rs` の `Transport::WtH3` 分岐で、`Settings::new().wt_enabled(1)` により旧 codepoint を広告している。
`crates/tokio-moq/src/webtransport_h3.rs` の `ClientConfig::enable_webtransport` は受け取った `Settings` を `enable_webtransport_client` に転送するだけで、版の選択は行わない。

canary.14 は `wt_enabled_draft17` と `DraftVersion::Draft17`、`ClientConnection::negotiated_wt_draft_version` を追加し、§4.3 の WT_STREAM の位置の規則も含む。canary.14 の `DraftVersion::build_client_settings(Draft17)` は初期フロー制御値を設定するため、フロー制御を広告しない現行方針にはそのまま使えない。

WtH3 の QUIC 層は tobi-quic を使うが、`crates/tokio-moq/src/quic/tobi.rs` の `build_transport_config` は `transport_params.reset_stream_at` を設定しておらず、tobi-quic の既定値は false である。このため WtH3 のクライアントは §3.1 が要求する空の `reset_stream_at` transport parameter を広告していない。peer の広告だけを `peer_supports_reset_stream_at` で確認している。

adapter の `wait_for_peer_settings` は `peer_settings().is_some()` だけを確認し、共通版の有無を見ない。`send_request` が返す `WtSetup(UnknownDraftVersion)` は `TransportError::Http3` になり、`is_retryable_connect_error` が再試行可能と分類する。
新 codepoint の広告と、旧 ID だけの peer の拒否を検証するテストは無い。

## 設計方針

対象依存を `shiguredo_http3 2026.1.0-canary.14` に更新する。更新対象は `crates/tokio-moq/Cargo.toml`、`crates/moq-pub/Cargo.toml`、`crates/moq-sub/Cargo.toml`、`Cargo.lock` である。

クライアントは draft-17 の draft 版として新 codepoint だけを広告し、旧 codepoint は広告しない。設定は `Settings::new().wt_enabled_draft17(VarInt::from_static(1))` で構築し、初期フロー制御値は 0 のままにする。0405 の HTTP/2 と同じく旧版との共存は選ばない。
この方針により、旧 ID だけの peer では共通版が無くなる。`wait_for_peer_settings` は peer の SETTINGS を受信した直後に `negotiated_wt_draft_version()` を確認し、`None` なら CONNECT を開く前に `TransportError::InvalidState` で失敗する。`send_request` の `UnknownDraftVersion` に任せて再試行可能な `TransportError::Http3` にしない。

WtH3 専用の transport config を追加し、`reset_stream_at` を true にしたうえで `crates/tokio-moq/src/webtransport_h3.rs` の接続構築で使う。ネイティブ QUIC の tobi バックエンドは従来の `build_transport_config` のままにする。§3.1 はクライアントとサーバーの双方に空の `reset_stream_at` transport parameter を求めるため、これがないと draft-17 の WebTransport 端点として接続できない。

adapter 側で版選択の規則を再実装しない。共通版の選択は依存の `negotiated_wt_draft_version()` に委ねる。Draft07 / Draft14 の Safari 互換の優先順位は、adapter がそれらを広告しないため対象外である。
`ClientConfig::enable_webtransport` のシグネチャは変更せず、受け取った `Settings` を転送するだけであることを doc コメントに明記する。

変更対象は次のとおり。

- `crates/tokio-moq/src/lib.rs` の `Transport::WtH3` 分岐の `Settings`
- `crates/tokio-moq/src/webtransport_h3.rs` の `wait_for_peer_settings`、モジュール doc、`test_local_settings` / `test_peer_settings` / `negotiated_connection_state`
- `crates/tokio-moq/src/quic/tobi.rs` の WtH3 専用 transport config と、`crates/tokio-moq/src/webtransport_h3.rs` の接続構築での使用
- `crates/tokio-moq/Cargo.toml`、`crates/moq-pub/Cargo.toml`、`crates/moq-sub/Cargo.toml`、`Cargo.lock`
- `crates/tokio-moq/README.md` の対応 draft (SETTINGS 交渉が draft-17 であることに限定し、draft-17 の全規定への準拠を主張しない)

コメントの更新は H3 の交渉に関係する箇所に限定する。§4.3 は canary.14 が対応するが、adapter 側のテストとコメント更新は 0409 で行う。§4.4 のアプリケーション offset は 0410 で扱う。0407 ではこれらの未完了の検証を draft-17 対応済みとして参照しない。

## 完了条件

- `0x20b8933b=1` だけを広告する peer と `DraftVersion::Draft17` を交渉し、CONNECT を開始できること。
- `0x2c7cf000=1` だけを広告する peer では `negotiated_wt_draft_version()` が `None` になり、CONNECT を開始せず `TransportError::InvalidState` で失敗すること。
- `0x2c7cf000=1` と `0x20b8933b=1` の両方を広告する peer では `DraftVersion::Draft17` を選び、旧 ID の SETTINGS を混用しないこと。
- クライアントのローカル SETTINGS が `0x20b8933b=1` を含み、`0x2c7cf000` を含まないこと。
- WtH3 の QUIC transport config が空の `reset_stream_at` を広告し、ネイティブ QUIC の tobi バックエンドは従来どおり広告しないこと。
- `crates/tokio-moq/Cargo.toml`、`crates/moq-pub/Cargo.toml`、`crates/moq-sub/Cargo.toml`、`Cargo.lock` が canary.14 を指し、H3 の交渉に関するコメントと `crates/tokio-moq/README.md` が draft-17 を参照すること。
- 検証は `crates/tokio-moq/src/webtransport_h3.rs` の `#[cfg(test)] mod tests` に追加する Sans I/O テストで行うこと。`ClientConnection` / `ServerConnection` と `peer_control_stream_bytes` を使い、WtH3 の実 QUIC peer ハーネスは追加しないこと。

## 差分の確認範囲

- §3.2 の前接続 SETTINGS のキャッシュ破棄は 0408、§4.3 の WT_STREAM の位置と終了コードは 0409、§4.4 の Reliable Size は 0410 で追跡する。
- §4.4 の Reliable Size について、現行の reset はアプリケーション offset を指定しないため、WebTransport header 長を Reliable Size にする現行動作は draft-17 の offset 0 相当として維持する。`RESET_STREAM_AT` の可用性は WtH3 では tobi-quic を使い、本 issue で `reset_stream_at` transport parameter を広告する。旧 s2n-quic の WtH3 経路の非対応 (0094) は別課題である。
- §5.6.2 / §5.6.4 の初期値を基準にする厳密増加は、固定中の依存の standalone な `webtransport::Session` では、フロー制御を有効にし、利用側が `initialize_remote_limits` を明示的に呼んだ後に `process_capsule` が `<=` を拒否する。
  adapter はこの Session API を使わず、既定設定ではフロー制御を宣言しないため該当 capsule は依存の Connection 層で無視される。
  本 issue の交渉更新でも既定のフロー制御無効を維持し、`ClientConfig::enable_webtransport` でフロー制御を広告する任意設定の受信経路まで対応済みとは扱わない。
- §4.6 の応答前 stream / datagram の拒否・drop と再送の説明は確認済み。adapter は 2xx 応答を待ってから session を返すため、optimistic な送信や自動再送を追加しない。
- §4.4 / §5 の intermediary のヘッダ長調整と上限転送は、この endpoint adapter の責務外である。
- §2.2 の HTTPS URI、§3.3 の重複文の削除、§8 の flow control 無効時の同居 HTTP との deadlock の説明、参照・IANA の変更も確認済み。現行 adapter は接続を専用に使い、他の HTTP 要求と多重化しない。
- サーバー側の `SETTINGS_WT_ENABLED` の無視規則と版選択の例外は、このクライアント専用 adapter の責務外である。依存クレートとテスト用 peer に任せる。

## 依存・関連

- 関連: [0408](0408-wt-h3-draft-17-resumption-current-settings.md)
- 関連: [0409](0409-wt-h3-draft-17-wt-stream-placement-error.md)
- 関連: [0410](0410-wt-h3-draft-17-reset-reliable-application-offset.md)
- 後続の 0408 / 0409 / 0410 は本 issue の依存更新後に着手する。

## 解決方法

`shiguredo_http3` を canary.14 に更新し、WtH3 のクライアント SETTINGS が draft-17 の `SETTINGS_WT_ENABLED` (`0x20b8933b`) だけを広告するようにした。旧 codepoint (`0x2c7cf000`) は広告しない。

- `crates/tokio-moq`, `crates/moq-pub`, `crates/moq-sub` の依存と `Cargo.lock` を canary.14 に更新した
- `wt_h3_settings()` を追加し、本番の接続経路とテストが同じ設定構築を使うようにした
- `wait_for_peer_settings` で `negotiated_wt_draft_version()` を確認し、共通版が無い peer は `InvalidState` で失敗するようにした
- WtH3 専用の transport config で `reset_stream_at` を広告するようにした
- 旧 codepoint だけの peer、新旧両方の peer、新 codepoint だけの peer のテストを追加した
