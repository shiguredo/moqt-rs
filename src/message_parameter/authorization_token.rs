//! AUTHORIZATION_TOKEN の Token 構造 (draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression))
//!
//! 公開型 [`AuthorizationToken`](super::AuthorizationToken) の定義は親モジュール
//! (`crate::message_parameter`) に置き、本モジュールはその `impl` だけを持つ。
//! 外部へは公開しない。
use super::AuthorizationToken;
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

impl core::fmt::Debug for AuthorizationToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Token Value は認可情報そのものであり、ログに残ると漏えいになる。前例
        // (`MsfFragmentParameter` / `MsfUri` / `tokio_moq::ServerUrl`) に揃え、実値の代わりに
        // バイト長を表示する
        // フィールドを追加したときに表示漏れをコンパイラが検出できるよう、`..` を使わずに分解する
        match self {
            Self::Delete { alias } => f.debug_struct("Delete").field("alias", alias).finish(),
            Self::Register {
                alias,
                token_type,
                token_value,
            } => f
                .debug_struct("Register")
                .field("alias", alias)
                .field("token_type", token_type)
                .field("token_value_bytes", &token_value.len())
                .finish(),
            Self::UseAlias { alias } => f.debug_struct("UseAlias").field("alias", alias).finish(),
            Self::UseValue {
                token_type,
                token_value,
            } => f
                .debug_struct("UseValue")
                .field("token_type", token_type)
                .field("token_value_bytes", &token_value.len())
                .finish(),
        }
    }
}

impl AuthorizationToken {
    /// Length-prefixed バイト列から Token 構造をデコードする
    ///
    /// draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression): "If the Token
    /// structure cannot be decoded, including an unknown Alias Type, the receiver MUST close
    /// the Session with MALFORMED_AUTH_TOKEN." (MUST) に従い、デコード失敗時は
    /// MALFORMED_AUTH_TOKEN を返す。
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let mut pos = 0;
        let (alias_type, n) = varint::decode(bytes)
            .map_err(|_| MessageError::MalformedAuthToken("failed to decode Token Alias Type"))?;
        pos += n;

        match alias_type {
            // DELETE (0x0): Token Alias のみ
            0 => {
                let (alias, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::MalformedAuthToken("failed to decode Token Alias in DELETE")
                })?;
                pos += n;
                if pos != bytes.len() {
                    return Err(MessageError::MalformedAuthToken(
                        "DELETE token has trailing bytes",
                    ));
                }
                Ok(Self::Delete { alias })
            }
            // REGISTER (0x1): Token Alias、Token Type、Token Value の順
            1 => {
                let (alias, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::MalformedAuthToken("failed to decode Token Alias in REGISTER")
                })?;
                pos += n;
                let (token_type, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::MalformedAuthToken("failed to decode Token Type in REGISTER")
                })?;
                pos += n;
                // Token Value は残りのバイト列すべて
                let token_value = bytes[pos..].to_vec();
                Ok(Self::Register {
                    alias,
                    token_type,
                    token_value,
                })
            }
            // USE_ALIAS (0x2): Token Alias のみ
            2 => {
                let (alias, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::MalformedAuthToken("failed to decode Token Alias in USE_ALIAS")
                })?;
                pos += n;
                if pos != bytes.len() {
                    return Err(MessageError::MalformedAuthToken(
                        "USE_ALIAS token has trailing bytes",
                    ));
                }
                Ok(Self::UseAlias { alias })
            }
            // USE_VALUE (0x3): Token Type + Token Value (Alias なし)
            3 => {
                let (token_type, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::MalformedAuthToken("failed to decode Token Type in USE_VALUE")
                })?;
                pos += n;
                let token_value = bytes[pos..].to_vec();
                Ok(Self::UseValue {
                    token_type,
                    token_value,
                })
            }
            _ => Err(MessageError::MalformedAuthToken("unknown Token Alias Type")),
        }
    }

    /// alias 解決なしで (Token Type, Token Value) を返す
    ///
    /// REGISTER / USE_VALUE の場合は Some を返し、DELETE / USE_ALIAS の場合は None を返す。
    /// USE_ALIAS の alias 解決はセッション状態に依存するため、コーデック層では不可能。
    pub(crate) fn token_type_value(&self) -> Option<(u64, &[u8])> {
        match self {
            Self::Register {
                token_type,
                token_value,
                ..
            } => Some((*token_type, token_value)),
            Self::UseValue {
                token_type,
                token_value,
            } => Some((*token_type, token_value)),
            Self::Delete { .. } | Self::UseAlias { .. } => None,
        }
    }

    /// SETUP の AUTHORIZATION_TOKEN として許可されるか検証する
    ///
    /// draft-ietf-moq-transport-23 §9.1.4 (AUTHORIZATION TOKEN): "If a server receives Alias
    /// Type DELETE (0x0) or USE_ALIAS (0x2) in a SETUP message, it MUST close the session with
    /// ROLE_VIOLATION." encode / decode の両経路で同一の検証を行うため本メソッドに集約する。
    /// 仕様が MUST を課すのは server 受信時だが、SETUP は最初のメッセージであり DELETE /
    /// USE_ALIAS が参照できる登録済み Alias は存在しないため、client 受信時も同じ分類
    /// (送信者の role では送れないフィールド) とする。
    pub(crate) fn validate_setup_scope(&self) -> Result<(), MessageError> {
        match self {
            Self::Delete { .. } => Err(MessageError::RoleViolation(
                "DELETE alias type is not allowed in SETUP",
            )),
            Self::UseAlias { .. } => Err(MessageError::RoleViolation(
                "USE_ALIAS alias type is not allowed in SETUP",
            )),
            _ => Ok(()),
        }
    }

    /// Token 構造をバイト列にエンコードする
    pub(crate) fn encode_to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Self::Delete { alias } => {
                varint::encode(0, &mut buf);
                varint::encode(*alias, &mut buf);
            }
            Self::Register {
                alias,
                token_type,
                token_value,
            } => {
                varint::encode(1, &mut buf);
                varint::encode(*alias, &mut buf);
                varint::encode(*token_type, &mut buf);
                buf.extend_from_slice(token_value);
            }
            Self::UseAlias { alias } => {
                varint::encode(2, &mut buf);
                varint::encode(*alias, &mut buf);
            }
            Self::UseValue {
                token_type,
                token_value,
            } => {
                varint::encode(3, &mut buf);
                varint::encode(*token_type, &mut buf);
                buf.extend_from_slice(token_value);
            }
        }
        buf
    }
}
