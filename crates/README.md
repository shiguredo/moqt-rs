# MoQ サンプル

Media over QUIC の publisher / subscriber クライアントのサンプルです。
draft-ietf-moq-transport-23、draft-ietf-moq-loc-05、draft-ietf-moq-msf-02、draft-ietf-moq-c4m-02 に準拠しています。

- [moq-pub](moq-pub/README.md)：catalog / audio / video track を PUBLISH する publisher
- [moq-sub](moq-sub/README.md)：catalog と audio / video を SUBSCRIBE して再生する subscriber
- [tokio-moq](tokio-moq/README.md)：publisher / subscriber が共有するトランスポート層 (ライブラリ)
- [happy-eyeballs](happy-eyeballs/README.md)：Sans I/O の Happy Eyeballs v3 接続候補スケジューラ

[Tokio](https://github.com/tokio-rs/tokio)、[tobi-quic](https://github.com/shiguredo/tobi-quic)、[http3-rs](https://github.com/shiguredo/http3-rs)、[http2-rs](https://github.com/shiguredo/http2-rs)、[qmux-rs](https://github.com/shiguredo/qmux-rs) を利用しています。
QUIC と WebTransport over HTTP/3 の実装は tobi-quic を使います。tobi-quic は Linux / macOS /
Windows に対応します。Multipath QUIC は Windows では使えません。詳細は
[tokio-moq](tokio-moq/README.md) を参照してください。
接続先となる MoQ リレーは別途用意してください。

Rust 1.95 以降が必要です (ライブラリの MSRV と tobi-quic の要求)。macOS ではカメラとマイクへのアクセス許可が必要です。

## 実行例

```bash
# publisher (疑似キャプチャ)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device

# subscriber
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example

# 自己署名証明書を使うローカル relay で試す (開発用。証明書検証をスキップする)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --insecure --fake-capture-device
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --insecure
```

## 接続設定

`--url`、`--transport`、`--namespace`、C4M 認可トークン、TLS 証明書の扱いは publisher と subscriber で共通です。詳細は [tokio-moq](tokio-moq/README.md) を参照してください。

- 接続経路は `--url` の scheme ではなく `--transport` (`quic` / `wt-h3` / `wt-h2` / `qmux` / `auto`) で選ぶ。省略時は MSF fragment の `connection` (`q` / `wt`)、それも無ければ `quic`
- `auto` は Happy Eyeballs v3 で複数の経路を競争する experimental な指定である。候補の並びは [tokio-moq](tokio-moq/README.md) を参照する
- `wt-h3` / `wt-h2` は `--transport wt-h3` / `--transport wt-h2` で選ぶ。`wt-h3` の QUIC 層は tobi-quic である。`wt-h2` は `WT_RESET_STREAM` capsule を使うため MoQ リレーの capsule 対応に依存する
- `qmux` は TCP + TLS 上で QUIC のストリームと DATAGRAM を多重化する経路で、TCP の再送による遅延や DATAGRAM の破棄があり得る
- Windows 以外のビルドでは、native QUIC 経路で Multipath QUIC (RFC 10069) を使える。既定は無効で、`--multipath` (`off` / `handover` / `aggregate` / `backup`) を指定したときだけ有効になる。詳細は [docs/multipath.md](../docs/multipath.md) を参照する
- `--namespace` を省略すると MSF fragment の track-identifier の namespace を使う。どちらも無ければエラー
- MSF fragment の `cat` は SETUP と PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE の AUTHORIZATION_TOKEN として送信する
- TLS は既定で OS のシステムルートストアによる証明書検証を行う。`--cert` で CA 証明書を指定すると、その証明書だけをルートとして検証する
- 自己署名証明書を使うローカル開発では `--insecure` を明示すると証明書検証をスキップできる (接続時に警告ログが出る)。`--cert` との同時指定はエラーになる。本番環境では `--insecure` を使わない

```bash
# C4M トークン付き (シェルでは & を引用符で囲む)
cargo run -p moq-pub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video&cat=<base64 token>' --fake-capture-device
```

各サンプルのオプションは [moq-pub](moq-pub/README.md) / [moq-sub](moq-sub/README.md) と `--help` で確認できます。
