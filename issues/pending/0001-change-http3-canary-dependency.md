# examples の shiguredo_http3 canary 依存を安定版に切り替える

- Priority: Low
- Created: 2026-06-04
- Model: Opus 4.8
- Branch: feature/change-http3-canary-dependency

## pending にした理由

`shiguredo_http3` の安定版 (マイナーバージョン指定が可能なリリース) がまだ存在しないため、外部依存のリリース待ちで対応できない。安定版がリリースされ次第、本 issue を `issues/` へ戻して対応する。

## 目的

examples が依存する `shiguredo_http3 = "2026.1.0-canary.1"` は canary プレリリース・パッチ指定であり、AGENTS.md「バージョン番号はマイナーバージョンまで指定すること」に反する。安定版へ切り替える。

## 優先度根拠

Low。crates.io に canary が公開済みで clone ビルドは可能。publish 対象は本体クレートのみ (`include = ["/LICENSE", "/README.md", "/src/**"]`) のため公開パッケージには影響しない。他の shiguredo クレート (`shiguredo_aom = "2026.1"` 等) とは粒度が不揃い。

## 現状

- `examples/moqt-publisher/Cargo.toml:12`
- `examples/moqt-subscriber/Cargo.toml:14`
- `examples/moqt-transport/Cargo.toml:16`

いずれも `shiguredo_http3 = "2026.1.0-canary.1"`。

## 設計方針

`shiguredo_http3` の安定版リリース後に `"2026.1"` 形式へ変更する。

## 完了条件

- 3 つの examples の Cargo.toml が安定版のマイナー指定になる。
- examples がビルドできる。

## 解決方法

安定版リリース後にバージョン指定を更新する。
