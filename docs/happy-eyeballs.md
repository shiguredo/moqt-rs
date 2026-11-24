# Sans I/O Happy Eyeballs v3 スケジューラと接続競争

## 目的

`crates/happy-eyeballs` に Sans I/O の接続候補スケジューラを追加し、`tokio-moq` から
WebTransport over HTTP/3 / WebTransport over HTTP/2 / native QUIC / QMux の候補を
Happy Eyeballs Version 3 の手順で競争させ、最初にトランスポートが確立した経路を採用する。

これまでの `--transport` は 1 つの経路を接続前に確定していた。UDP が使えない環境では
native QUIC が失敗してから QMux へ切り替える必要があり、WebTransport でも HTTP/3 と
HTTP/2 のどちらが使えるかを事前に知る手段が無かった。複数の経路を短い間隔で並行に試し、
最初に確立した経路を採用すれば、利用者に経路を決めさせずに接続時間を短くできる。

## 実装の構成

- `crates/happy-eyeballs` は `#![no_std]` の Sans I/O クレートである。時刻は呼び出し側から
  渡す単調増加のミリ秒だけを使い、`std` / `tokio` / `Instant` / `JoinSet` / `TaskMonitor` /
  ソケット / DNS / ログを持たない
- `crates/tokio-moq` は I/O アダプタである。名前解決、接続 future、`JoinSet`、
  `tokio::time`、`Instant`、`TaskMonitor`、負け候補の close を担当する
- `shiguredo_moqt` は MOQT のコーデックとセッション状態機械のままで、このクレートに
  依存しない

一次資料は
[`refs/happy/draft-ietf-happy-happyeyeballs-v3-05.txt`](../refs/happy/draft-ietf-happy-happyeyeballs-v3-05.txt)
に置く。本実装は draft-ietf-happy-happyeyeballs-v3-05 (2026-11-02) の
Overview (§3)、Sorting Destination Endpoints Within Groups (§5.3)、Connection Attempts (§6)、
Determining successful connection establishment (§6.1)、Summary of Configurable Values (§10) に
従う。

## draft-05 との対応

| 節 | 内容 | 本実装 |
| --- | --- | --- |
| §3 | QUIC を TCP より優先する前提 | 候補順で QUIC 系を TCP 系より先にする |
| §4 | DNS の非同期解決と Resolution Delay | 対象外。`tokio::net::lookup_host` の 1 回の解決を使う |
| §5.1 | アプリケーションプロトコル / セキュリティ要件によるグループ化 | トランスポート単位の優先順として実装。SVCB の `alpn` / `ech` は扱わない |
| §5.2 | SVCB のサービス優先度によるグループ化 | 対象外。SVCB を取得しないため、トランスポートの優先順をアプリケーションが決める |
| §5.3 | 宛先アドレスのソートとファミリの交互配置 | 交互配置と重複除去を実装。RFC 6724 のソート、RTT / 利用履歴は解決器と将来の課題に委ねる |
| §6 | 接続試行と Connection Attempt Delay | 実装。失敗時の前倒しは本実装の拡張 |
| §6.1 | 成功判定とタイマーの調整 | 成功判定は実装。接続途中の進捗に応じたタイマー延長は未実装 |
| §6.2 | SVCB の ALPN に基づく試行の制限 | 対象外 |
| §6.3 | SVCB 応答待ちの試行の保留 | 対象外 |
| §7 | 接続中の DNS 応答の変化 | 対象外 |
| §8 | NAT64 / PREF64 | 対象外 |
| §9 | 複数ネットワークとネットワーク変更 | 対象外 |
| §10 | 設定可能な値 | 実装 (`happy_eyeballs::Config`。Sans I/O コアが保持する) |

§6 は `Next Connection Attempt Timer` で次の試行を開始すると定める。本実装は、試行の失敗を
観測したときに推奨最小値 (既定 100 ms) まで前倒しする。これは本実装の拡張であり、仕様の
MUST である 10 ms の下限は守る。

## 公開 API

- `Transport::Auto` (`--transport auto`) を追加する。既存の `quic` / `wt-h3` / `wt-h2` /
  `qmux` は単一経路の指定として維持する。既定は従来どおり `quic` であり、`auto` は
  明示したときだけ使う
- `resolve_transport` は `auto` をそのまま返す。候補の展開は接続時 (`connect`) に行う
- `ConnectParams::happy_eyeballs` (`happy_eyeballs::Config`) で、Connection Attempt Delay、
  推奨最小値、推奨最大値、Connection Attempt Delay Multiplier、Preferred Address Family
  Count を変更できる。既定値は §10 の推奨値である。CLI からはまだ変更できない

`Transport::Auto` の候補集合は、URL の `msf` fragment の `connection` とビルドで利用できる
経路で決まる。

| `connection` | 候補 (優先順) | 理由 |
| --- | --- | --- |
| 無し | `quic` → `wt-h3` → `qmux` → `wt-h2` | QUIC を TCP より優先する (§3)。同じ QUIC 系では層の少ない native QUIC を先にする |
| `q` | `quic` | draft-ietf-moq-msf-02 §11.1.1 が Native QUIC を MUST で要求する。QMux は TCP+TLS 上で QUIC のストリーム操作を多重化する経路であり、Native QUIC ではない |
| `wt` | `wt-h3` → `wt-h2` | WebTransport の実装を指定しない `wt` の既定を HTTP/3 とし、HTTP/2 をフォールバックにする |

`connection=q` と `connection=wt` の併記は従来どおりエラーである。

`connection=q` で QMux を競争させないのは、仕様の MUST を守るためである。QMux を
native QUIC と同じ接続種別として扱うかどうかは、QMux 上の MOQT を扱う individual draft
(`refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt` §2.2.1) が QUIC と QMux の競争を
MAY で示している一方、MSF は `q` を Native QUIC に限定している。本実装は MSF の MUST を
優先し、QMux を候補に含めるのは `connection` が無い場合だけにした。この判断は両 draft の
決着に合わせて見直す。

## 候補の並び順

1. トランスポートを上表の優先順に並べる
2. 各トランスポートについて、解決済みアドレスをアドレスファミリ交互の順序にする

アドレスファミリの並べ方は §5.3 に従う。先頭のアドレスのファミリを優先し、そのファミリの
アドレスを `Preferred Address Family Count` (既定 1、設定で変更可能) 個並べたあと、
もう一方のファミリのアドレスを 1 個挟む。ファミリ内の順序は `tokio::net::lookup_host` が返した順序をそのまま使う。OS の
解決器は RFC 6724 の宛先アドレス選択を適用済みであるため、本実装で並べ直さない。

同じアドレスが複数回解決された場合は 1 つに畳む。

ファミリ内の順序は解決器が返した順序を使う。`getaddrinfo` が常に RFC 6724 の宛先アドレス
選択を適用する保証はないため、そのような環境では §5.3 の MUST を満たさない。in-process の
宛先アドレス選択は将来の課題とする。

トランスポートごとに全アドレスを並べるため、先頭のトランスポートの全アドレスを試してから
次のトランスポートへ進む。§6 はサービス優先度のグループについて「先頭グループのすべての
候補を開始してから次のグループへ進む」と定め、アプリケーションの優先順についても同じ
扱いを許している。失敗した試行の次は前倒しで開始するため、先頭トランスポートの全滅を
待つ時間は短い。

## 試行のスケジュール

- 最初の候補は即座に開始する (§6)
- 以降の候補は `Next Connection Attempt Timer` で開始する。`Connection Attempt Delay` は
  既定 250 ms、推奨最小値 100 ms、推奨最大値 2 s、倍率 1 (§6 / §10)。値は
  `ConnectParams::happy_eyeballs` で変更できる
- 試行が失敗したときは、次の候補の開始を前倒しする。ただし、直前の試行の開始から
  `minimum_connection_attempt_delay` (既定 100 ms) 未満では開始しない。仕様の MUST である
  10 ms も下限として守る。失敗が分かった時点で次の候補へ進み、250 ms を待たないためで
  ある。前倒しの間隔には倍率を適用しない (倍率は通常のタイマーにだけ適用する)
- 既定の倍率 1 では、進行中の試行数が増えても遅延は一定である。
  `Connection Attempt Delay Multiplier` を 1 より大きくすると、進行中の試行が増えるたびに
  遅延が指数的に伸びる (§6)。ただし、基準値は `minimum_connection_attempt_delay` で
  切り上げられるため、仕様の例にある 50 ms の基準値を使うには最小値も 50 ms 以下に
  設定する必要がある

Sans I/O コアの `Scheduler` は、最初の候補の `Start`、以降の `WaitUntil`、失敗時の
前倒し、勝者の `Winner` を同期関数として返す。`tokio-moq` のアダプタが `Start` を
`JoinSet` の独立タスクへ、`WaitUntil` を `tokio::time::sleep_until` へ変換する。各試行は
独立した TLS / QUIC ハンドシェイクを持ち、鍵共有や QUIC の Initial パケットを候補間で
再利用しない (§6)。0-RTT とセッション再開は使わない。

## 成功判定と MOQT SETUP

成功判定は、QUIC / TLS のハンドシェイクと、WebTransport の Extended CONNECT および
プロトコル交渉が完了した時点とする (§6.1 は成功判定をアプリケーションに委ねる)。
MOQT の SETUP は競争の勝者だけで交換する。

QMux は TLS + ALPN とローカルの QMux 接続生成だけでは成功とせず、ドライバ起動後にピアの
`QX_TRANSPORT_PARAMETERS` を受信するまで待つ。現行の qmux-rs 公開 API に待機用の関数が
無いため、10 ms 間隔で確認して 1 秒で打ち切る。受信できない peer は再試行可能な接続失敗
として扱い、次の候補へ進む。

SETUP を競争に含めない理由は、候補ごとに MOQT セッションを確立すると、同じ認可トークンと
同じ SETUP を複数の接続へ送ることになり、サーバー側のセッション数と認可の副作用が増える
ためである。SETUP が失敗した場合は、その接続を破棄してエラーを返し、別の候補へは
フォールバックしない。これは従来の「アドレスを変えて SETUP を送り直さない」方針と同じで
ある。

## キャンセルと後始末

いずれかの試行が成功した時点で、残りの試行タスクを中断する。中断したタスクの drop が
完了するまで待ってから勝者の接続を返す。中断時点ですでに確立まで完了していた負け候補は、
`ConnectedTransport::close` で接続を閉じる。負けた試行のソケット、TLS ストリーム、QUIC
エンドポイント、WebTransport のドライバタスクが idle timeout まで残らないようにするため
である。

WebTransport over HTTP/3、QMux、tobi-quic の native QUIC は、接続ハンドルを保持する
タスクが残ると driver が終了しない。これらのセッション型は drop 時に接続を閉じるガードを
持ち、競争の勝敗やセッションの終了にかかわらず接続を閉じる。

`JoinSet` の drop だけでもタスクは中断されるが、`abort_all` のあと `join_next` で完了を
待つ。接続試行のタスクが panic した場合は、残りの試行を中断して `RaceFailure::Task` を
直ちに返す。panic を通常の接続失敗として再試行しないためである。

WebTransport over HTTP/3 は CONNECT の途中で受信タスクを起動する。tobi-quic の接続は
接続ハンドルを保持するタスクが残る間はドライバが終了しないため、`WtClient::connect` は
中断時に接続を明示的に閉じるガード (`ConnectionCloseGuard`) を持つ。確立に成功した
場合はガードを `WtSession` が所有し、セッションの drop 時に接続を閉じる。

## エラー

すべての候補が失敗した場合は、候補の並び (先頭ほど優先) で最も先頭の失敗を返す。完了順で
結果が変わらないようにするためである。各試行の失敗は `warn` ログに経路とアドレス付きで
残すため、最終エラーだけでは分からない失敗の内訳はログから追える。

候補が 1 つも無い場合は `TransportError::Unsupported` にする。`connection=q` と
`connection=wt` の併記など、`connection` の解釈エラーは `TransportError::InvalidState` に
する。

## 対象外

- DNS の非同期解決 (§4): `tokio::net::lookup_host` は A / AAAA をまとめて返す API であり、
  レコード種別ごとの非同期処理と Resolution Delay を実装できない。解決は 1 回の
  システムコールに委ねる
- SVCB / HTTPS レコードの解釈 (§5.1 / §5.2 / §6.2 / §6.3): 本クレートは DNS の
  サービス情報を取得しないため、ALPN の集合やサービス優先度による候補の絞り込みは行わない
- RFC 6724 の宛先アドレス選択: OS の解決器の結果を使う
- NAT64 の PREF64 と合成アドレス (§8): 対象外
- 複数ネットワークとネットワーク変更 (§9): 対象外
- 接続確立後の経路品質の評価と接続の切り替え (§6 の後段): 対象外
- 接続試行の途中経過 (TCP ハンドシェイク完了後の TLS など) に合わせた
  `Next Connection Attempt Timer` の延長 (§6.1): 接続確立全体を 1 つの future として
  扱うため行わない
- 0-RTT とセッション再開: 対象外 (既存実装と同じ)

## テスト

- `crates/happy-eyeballs` の単体テストで、アドレスファミリの交互配置、重複除去、
  トランスポート優先順、遅延の範囲、倍率の適用、設定値の丸めと極端な Duration、
  最初の成功の採用、失敗時の前倒し、全滅時の候補優先エラー、候補が空の場合、
  状態機械の不正なイベントを固定する
- `pbt/tests/prop_happy_eyeballs.rs` で、アドレス集合の保存と設定の安全範囲を検査する
- `lib.rs` の単体テストで、`Transport::parse("auto")`、`resolve_transport` と
  `candidate_transports` の `connection` ごとの候補集合を固定する
- `crates/tokio-moq/src/lib.rs` の結合テストで、QUIC の待ち受けが無いポートに対して
  QMux のテストサーバーだけを立て、`Transport::Auto` が QMux にフォールバックして
  SETUP まで完了することを実接続で確かめる
- `crates/tokio-moq/src/qmux.rs` のテストで、ピアが `QX_TRANSPORT_PARAMETERS` を送らない
  場合に QMux の成功判定がタイムアウトすることと、QMux のドライバが close で終了することを
  確かめる

モックやスタブは使わない。競争のテストは実在の TCP リスナーと、`peer_harness` の実サーバー
だけを使う。

## 残る課題

- QMux の `QX_TRANSPORT_PARAMETERS` 待ちは、qmux-rs に待機 API が無いため 10 ms の
  ポーリングである。待機 API が追加されたら置き換える
- `connection=q` で QMux を候補に含められない。UDP が使えない環境で QMux へ
  フォールバックしたい場合は、URL から `connection=q` を外す必要がある
- SETUP の失敗は候補の失敗として扱わない。MOQT の応答まで含めた競争は、認可トークンの
  扱いとセッション数の管理を設計してから導入する
- `Connection Attempt Delay` などの値は `ConnectParams::happy_eyeballs` で変更できるが、
  CLI からはまだ変更できない
- 候補集合を呼び出し側が明示する API は無い。`Transport::Auto` は `connection` とビルドで
  利用できる経路から候補を決める
- MSF の `connection=wt` は h2 / h3 を区別しない。URL で h2 / h3 を指定する仕様拡張や
  SVCB / HTTPS レコードの `alpn` 利用は、MSF / HEv3 の議論を見てからにする。現時点では
  `--transport auto` と `--transport wt-h2` / `wt-h3` でクライアント側から選ぶ
- DNS の Resolution Delay と SVCB / HTTPS は未対応である。将来 SVCB / HTTPS を扱う
  DNS クレートを導入する場合は、候補を `SvcParam` の ALPN と priority で絞り込む余地がある
