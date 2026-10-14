# MoQ サンプル

Media over QUIC の publisher / subscriber クライアントのサンプル。

draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01、draft-ietf-moq-c4m-01 に準拠。

- [moq-pub](moq-pub/README.md)：catalog / audio / video track を MoQ リレーへ PUBLISH する publisher クライアント
- [moq-sub](moq-sub/README.md)：MoQ リレーから catalog を SUBSCRIBE して取得し、audio / video を SUBSCRIBE してデコードして再生する subscriber クライアント
- [tokio-moq](tokio-moq/README.md)：publisher / subscriber が共有するトランスポート層 (ライブラリ)

publisher / subscriber の接続先となる MoQ リレーは別途用意する。
Rust のバージョン前提はリポジトリルートの `README.md` を参照すること。
macOS ではカメラとマイクへのアクセス許可が必要である。

## 実行例

```bash
# QUIC で publisher を起動 (疑似キャプチャ)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device

# QUIC で subscriber を起動
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example
```
