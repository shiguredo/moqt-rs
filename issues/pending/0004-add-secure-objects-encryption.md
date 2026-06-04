# Secure Objects による E2E 暗号化に対応する

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-secure-objects-encryption
- Polished: {YYYY-MM-DD}

## pending にした理由

MSF-01 §4.3 (Content protection and encryption) と LOC-04 §3 (Payload Encryption) は、暗号化方式を [SecureObjects] (draft-jennings-moq-secure-objects) に委譲している。しかし `refs/` に当該ドラフトが無いため、wire 形式・鍵導出・AAD 構築・暗号スイートの節番号や文面を一次資料で確認できない。推測で暗号処理を実装すると相互運用と安全性の両方を損なうため保留する。

加えて以下の設計判断が必要である。

- 暗号ライブラリ (規約では aws-lc-rs) の追加。本クレートは `no_std` を掲げており、`no_std` 対応の可否と feature 分岐の設計が必要
- MSF-01 §4.3.3 は「content encryption をサポートする実装は moq-secure-objects を実装 MUST」とするが、本ライブラリは content encryption 自体をサポートしないため、この MUST は現状適用外である。対応するか否かの方針確定が必要
- LOC Private Properties (MSF-01 §4.3.4 の type 0xA) と LOC Public Properties の暗号化境界の設計

[SecureObjects] を `refs/` に追加して節番号・文面を確定した後に、本 issue を `issues/` へ戻して対応する。

## 目的

MSF-01 §4.3 / LOC-04 §3 の E2E 暗号化に対応し、暗号化された LOC payload を relay を介して送受信できるようにする。

## 現状

- `src/msf.rs` の `MsfTrack` は `encryption_scheme` / `cipher_suite` / `key_id` / `track_base_key` の signaling フィールドを持つが、これは catalog のメタデータのみ
- `src/loc.rs` は `LocProperties` / `LocPropertyValue` によるプロパティ codec のみで、暗号化・復号・AAD 構築・鍵導出は無い
- `Cargo.toml` に暗号ライブラリの依存は無い
- `refs/moq/` に secure-objects ドラフトが無い (LOC-04 の [SecureObjects] 参照は draft-ietf-moq-secure-objects-01、MSF-01 は -04 を参照)

## 設計方針

- [SecureObjects] の Key ID / Full Track Name / Immutable Properties / Group ID / Object ID / Publisher Priority を AAD に含める規則に従う
- MSF-01 §5.2.39 の cipher suite (`aes-128-gcm-sha256` MUST、`aes-128-ctr-hmac-sha256-80` SHOULD) と LOC-04 §3.1.4 の `AES_128_GCM_SHA256_128` (0x0004) の対応を整理する
- LOC-04 §3.1.3 の Private Properties (timestamp / timescale / audio level / video frame marking / video config) の暗号化と、復号後の再構成を実装する
- 鍵導出は trackBaseKey と track name から行う ([SecureObjects] §5)

## 完了条件

- [SecureObjects] の仕様を `refs/` に追加した上で、Key ID / AAD / 鍵導出 / AEAD を実装すること
- LOC Private Properties を含む payload の暗号化と復号が roundtrip すること
- MSF-01 §5.2.38-§5.2.41 の signaling と整合すること
- `tests/` / `pbt/` に対応テストが追加されていること
