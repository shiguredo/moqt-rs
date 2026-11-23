# README と SKILL.md に CMSF draft-01 対応を記載する

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: {YYYY-MM-DD}

## 目的

CMSF 対応 (0467〜0473) が揃ったら、利用者向けの入口になる文書に CMSF draft-01 を対応仕様として載せる。0437 (MSF の版表記) や 0420 (C4M の文書) と同じく、機能の issue とは分けて最後に更新する。

次の文書は、それぞれの機能の issue が自分の分を更新するので、本 issue の対象外である。

- `docs/msf.md` の CMSF の節
- `crates/moq-pub/README.md` (0471)
- `crates/moq-sub/README.md` (0472)

## 現状

- `README.md` の「対応仕様」には、MoQT draft-23 / LOC draft-05 / MSF draft-02 / C4M draft-02 が載っている。CMSF は載っていない。
- `skills/shiguredo-moqt/SKILL.md` は、ライブラリ `shiguredo_moqt` の機能と API のリファレンスである。CMSF の型の記載はない。moq-pub / moq-sub の CLI は扱っていない。CLI の説明は `crates/moq-pub/README.md` / `crates/moq-sub/README.md` にあり、0471 / 0472 が更新する。

## 設計方針

- `README.md` の対応仕様に、CMSF draft-01 (`https://datatracker.ietf.org/doc/html/draft-ietf-moq-cmsf-01`) を追加する。表記は既存の MSF 行に合わせる。
- `skills/shiguredo-moqt/SKILL.md` には、ライブラリの型と API だけを追記する。内容は 0467〜0470 で実装した API に合わせる。
  - CMSF の型と定数: `MsfPackaging::Cmaf`、`CmsfContentProtection`、`CmsfDrmSystem`、`CmsfSapTypeRecord`、`CMSF_SAP_TYPE_EVENT_TYPE`
  - 追加したフィールド: `MsfTrack` の SAP starting type と `content_protection_ref_ids`、`MsfCatalog::content_protections`。SKILL.md の `MsfTrack` のフィールド一覧も更新する
  - 変更した API: `MsfTrack::stream_mapping` の戻り値 (0468)

## 完了条件

- `README.md` の対応仕様に CMSF draft-01 があること。
- `skills/shiguredo-moqt/SKILL.md` の CMSF の記載が、実装した API に一致していること。CLI の記載を加えていないこと。
