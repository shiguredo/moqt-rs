//! FETCH 関連のテスト群
//!
//! FETCH (統一形式) / fill fetch stream / REQUEST_UPDATE / バリデーションの
//! 責務別にサブモジュールへ分割する。
//! (draft-ietf-moq-transport-21 で Joining FETCH は廃止され、
//! FETCH は Standalone / Joining バリアントのない単一形式になった)

use super::*;

#[path = "fetch/after_fin.rs"]
mod after_fin;
#[path = "fetch/fill.rs"]
mod fill;
#[path = "fetch/request_update.rs"]
mod request_update;
#[path = "fetch/unified.rs"]
mod unified;
#[path = "fetch/validation.rs"]
mod validation;
