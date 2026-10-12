# カタログの track 単位 namespace を購読に反映する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-track-namespace-override
- Polished: 2026-10-11
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
  `MoqtClient::subscribe_track` を呼ぶ。
- `examples/moq-sub/src/catalog.rs` の `CatalogState::apply` は delta の適用時に catalog の
  namespace を渡す (namespace 継承の扱いは 0218 で対応済み)。本 issue は購読に使う
  namespace の解決を扱い、delta の継承とは別である。
- そのため、カタログの track が catalog track と異なる namespace を宣言している場合に誤った
  namespace を購読する。example の publisher (moq-pub) は全 track に同じ namespace を書くため
  現状の組み合わせでは顕在化しない。

## 設計方針

- `VideoTrackInfo` / `AudioTrackInfo` に購読に使う namespace を持たせ、`MsfTrack::namespace` が
  あればそれを使い、無ければ catalog track の namespace を継承する。
- namespace の表現は draft-ietf-moq-transport-22 §8.8 (Representing Namespace and Track Names。
  MSF では §11.1.2 に再掲) の文字列であり、
  `shiguredo_moqt::name::parse_namespace` で `TrackNamespace` へ解決する。
- MSF §5.2.2 は catalog JSON の namespace を String と定めるのみで符号化を規定しない
  (§11.1.2 は URL fragment 用)。本 example は moq-pub が書く §8.8 表現を前提とし、
  それ以外の表現のカタログは解決失敗として扱う (0218 と同じ前提)。
- 解決に失敗したときはエラーで終了する。カタログは publisher の契約であり、§8.8 として
  不正な namespace は publisher のバグである。黙ってスキップすると、ユーザーが期待した
  track が購読されず、別の track の購読や DOES_NOT_EXIST と同じく気づかない誤動作になる。
  既存の `extract_video_info` / `extract_audio_info` も必須フィールド欠如でエラーにする
  ため、挙動を揃える。video / audio のどちらか一方だけ解決できない場合もエラーで終了する
  (片方だけスキップして再生を続けると、ユーザーが期待した映像または音声が欠ける)。
  この扱いは doc とテストで固定する。
- track の選択に使う codec 判定は変えない (example は track の role を参照していない)。

## 完了条件

- namespace を宣言した track を、宣言された namespace で購読することのテスト。
- namespace を宣言しない track を catalog track の namespace で購読することのテスト。
- 解決できない namespace の扱いが doc とテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

- `examples/moq-sub/src/pipeline.rs` の `VideoTrackInfo` / `AudioTrackInfo` に購読に使う
  `namespace: TrackNamespace` を追加した。
- `resolve_track_namespace(track, inherited)` を追加し、`MsfTrack::namespace` を宣言していれば
  `shiguredo_moqt::name::parse_namespace` で解決し (draft-ietf-moq-transport-22 §8.8 の表現。
  MSF では §11.1.2 に再掲)、宣言が無ければ catalog track の namespace を継承する
  (draft-ietf-moq-msf-01 §5.2.2)。解決に失敗した場合はエラーにした。黙って継承すると別の track を
  購読して気づかない誤動作になるためである (video / audio の片方だけ解決できない場合もエラー)。
  空の namespace (0 フィールド) も、example が 1 つ以上のフィールドを持つ namespace を扱う前提
  (`tokio_moq::resolve_namespace` と同じ) から解決失敗として扱う。§8.8 の形式は RECOMMENDED で
  あり MSF も符号化を規定しないため、本 example が §8.8 表現を前提とする契約であること、それ以外の
  表現 (MSF §5.6.1 の例のような値) では起動できないことを doc に書いた。解決は `--no-video` /
  `--no-audio` で無効化した track でも行う (既存の必須フィールドの検証と同じ扱い。検証は購読対象と
  して選択した映像 1 本・音声 1 本に対して行う)。
- `extract_video_info` / `extract_audio_info` が継承元の namespace を受け取り、解決結果を
  `VideoTrackInfo` / `AudioTrackInfo` に入れるようにした。video / audio の `subscribe_track` は
  解決済みの namespace を使う。この配線は、`run` が `config.namespace` を move して受け取り
  `receive_catalog` へも move で渡すため、購読呼び出しを catalog track の namespace に変える変更が
  コンパイルエラー (`E0382`) になることで守られる (move の受け取りごと元に戻す 2 行以上の変更は
  防げない)。
- `receive_catalog` の引数を §8.8 表現の `String` から `TrackNamespace` に変え、catalog の delta の
  継承判定に使う §8.8 表現への `serialize_namespace` は関数内へ移した (継承元を parse し直す往復を
  避ける)。購読のログに namespace を追加した。
- テストは 8 本追加した。宣言 namespace の優先、catalog track の namespace の継承、解決できない
  namespace と空の namespace のエラー (video / audio それぞれ) を固定する。変異テストで、
  `resolve_track_namespace` が宣言を無視して継承を返す変異で 4 本、解決失敗を継承で握りつぶす変異で
  2 本、空 namespace の拒否を削除する変異で 2 本が失敗することを確認した (宣言を無視する変異を
  `track.namespace.as_deref()` を常に `None` にする形にすると 6 本が失敗する)。
- 購読呼び出しそのものを実行する pipeline レベルの結合テストは本 issue の対象外とする。
  `examples/tokio-moq/src/peer_harness.rs` は `#[cfg(test)]` のため `moq-sub` から使えず、
  e2e-tests は実 relay 前提で、moq-pub は全 track に同じ namespace を書くため track 単位の namespace
  上書きを再現できない (0242 の解決方法が「`moq-pub` / `moq-sub` から使えるようにする変更は本 issue
  では行わない」と明記している)。moq-pub に track 単位の namespace を書かせる経路と合わせて別途
  対応する。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。
