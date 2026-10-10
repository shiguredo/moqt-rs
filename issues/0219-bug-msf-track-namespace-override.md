# カタログの track 単位 namespace を購読に反映する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-track-namespace-override
- Polished: {YYYY-MM-DD}
- Updated: 2026-10-10

## 目的

draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は「A namespace declared in a track object overrides
any inherited name space.」と定める。track object が namespace を宣言している場合、その track の
Full Track Name は (宣言された namespace, Track Name) であり、catalog track の namespace を
継承したものではない。MOQT の購読は Full Track Name に対して行うため
(draft-ietf-moq-transport-22 §2.4.1 (Track Naming))、宣言 namespace を使わないと別の track を
購読するか DOES_NOT_EXIST になる。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `extract_video_info` / `extract_audio_info` は
  `MsfTrack::name` や codec などだけを取り出し、`MsfTrack::namespace` を参照しない。
- 同じファイルの `run` は catalog track の namespace (`config.namespace`、CLI または
  MSF fragment の track-identifier 由来) をそのまま使って
  `MoqtClient::subscribe_track` / `MoqtClient::subscribe_track_with_filter` を呼ぶ。
- `examples/moq-sub/src/catalog.rs` の `CatalogState::apply` も、delta の適用時に catalog の
  namespace を渡していない (namespace 継承の扱いは 0218 で対応する)。
- そのため、カタログの track が catalog track と異なる namespace を宣言している場合に誤った
  namespace を購読する。example の publisher (moq-pub) は全 track に同じ namespace を書くため
  現状の組み合わせでは顕在化しない。

## 設計方針

- `VideoTrackInfo` / `AudioTrackInfo` に購読に使う namespace を持たせ、`MsfTrack::namespace` が
  あればそれを使い、無ければ catalog track の namespace を継承する。
- namespace の表現は draft-ietf-moq-transport-22 §8.8 (Representing Namespace and Track Names。
  MSF では §11.1.2 に再掲) の文字列であり、
  `shiguredo_moqt::name::parse_namespace` で `TrackNamespace` へ解決する。
- 解決に失敗したときの扱い (警告してその track をスキップする / エラーで終了する) を決めて
  doc に書く。track の選択は video / audio の 1 つずつであり、片方だけ解決できない場合の
  挙動も決める。
- track の選択に使う codec 判定は変えない (example は track の role を参照していない)。

## 完了条件

- namespace を宣言した track を、宣言された namespace で購読することのテスト。
- namespace を宣言しない track を catalog track の namespace で購読することのテスト。
- 解決できない namespace の扱いが doc とテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
