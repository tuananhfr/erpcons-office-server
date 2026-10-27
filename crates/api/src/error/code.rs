use axum::http::StatusCode;
use serde::Serialize;

/// `Error.code` of the OpenAPI contract. Clients branch on it; the message is for people.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthenticated,
    NotFound,
    NameTaken,
    FolderCycle,
    VersionConflict,
    PreconditionRequired,
    QuotaExceeded,
    TooLarge,
    UnsupportedType,
    InvalidSignature,
    InvalidRequest,
    IdempotencyMismatch,
    RateLimited,
    Unavailable,
    MethodNotAllowed,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 15] = [
        Self::Unauthenticated,
        Self::NotFound,
        Self::NameTaken,
        Self::FolderCycle,
        Self::VersionConflict,
        Self::PreconditionRequired,
        Self::QuotaExceeded,
        Self::TooLarge,
        Self::UnsupportedType,
        Self::InvalidSignature,
        Self::InvalidRequest,
        Self::IdempotencyMismatch,
        Self::RateLimited,
        Self::Unavailable,
        Self::MethodNotAllowed,
    ];

    pub fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::NameTaken | Self::VersionConflict => StatusCode::CONFLICT,
            Self::PreconditionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::QuotaExceeded | Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::FolderCycle
            | Self::UnsupportedType
            | Self::InvalidSignature
            | Self::InvalidRequest
            | Self::IdempotencyMismatch => StatusCode::UNPROCESSABLE_ENTITY,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        }
    }

    pub fn message_vi(self) -> &'static str {
        match self {
            Self::Unauthenticated => {
                "Phiên đăng nhập đã hết hạn hoặc không hợp lệ. Vui lòng đăng nhập lại."
            }
            Self::NotFound => "Không tìm thấy tài liệu hoặc thư mục.",
            Self::NameTaken => "Đã có thư mục trùng tên ở vị trí này.",
            Self::FolderCycle => "Không thể chuyển thư mục vào chính nó hoặc thư mục con của nó.",
            Self::VersionConflict => "Tài liệu đã được lưu ở nơi khác.",
            Self::PreconditionRequired => "Thiếu phiên bản gốc của tài liệu, chưa thể lưu.",
            Self::QuotaExceeded => "Đã hết dung lượng lưu trữ.",
            Self::TooLarge => "Tệp vượt quá kích thước cho phép.",
            Self::UnsupportedType => "Định dạng tệp này chưa được hỗ trợ.",
            Self::InvalidSignature => "Nội dung tệp không khớp với định dạng của tệp.",
            Self::InvalidRequest => "Yêu cầu không hợp lệ.",
            Self::IdempotencyMismatch => "Yêu cầu này trùng mã với một yêu cầu khác đã gửi.",
            Self::RateLimited => "Bạn thao tác quá nhanh. Vui lòng thử lại sau giây lát.",
            Self::Unavailable => "Máy chủ đang tạm thời gián đoạn. Vui lòng thử lại sau.",
            Self::MethodNotAllowed => "Thao tác này không được hỗ trợ ở địa chỉ này.",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENAPI: &str = include_str!("../../../../docs/openapi.yaml");

    /// The `enum:` list of `components.schemas.Error.properties.code` in the contract.
    fn contract_codes() -> Vec<String> {
        let error = OPENAPI
            .split("\n    Error:\n")
            .nth(1)
            .expect("Error schema in openapi.yaml");
        let list = error.split("enum:\n").nth(1).expect("code enum");
        list.lines()
            .map_while(|l| l.trim().strip_prefix("- "))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn codes_match_the_openapi_contract() {
        let ours: Vec<String> = ErrorCode::ALL
            .iter()
            .map(|c| serde_json::to_value(c).expect("serialize"))
            .map(|v| v.as_str().expect("string").to_owned())
            .collect();
        assert_eq!(ours, contract_codes());
    }

    #[test]
    fn statuses_follow_the_contract() {
        assert_eq!(ErrorCode::Unauthenticated.status(), 401);
        assert_eq!(ErrorCode::NotFound.status(), 404);
        assert_eq!(ErrorCode::VersionConflict.status(), 409);
        assert_eq!(ErrorCode::PreconditionRequired.status(), 428);
        assert_eq!(ErrorCode::QuotaExceeded.status(), 413);
        assert_eq!(ErrorCode::IdempotencyMismatch.status(), 422);
        assert_eq!(ErrorCode::RateLimited.status(), 429);
        assert_eq!(ErrorCode::Unavailable.status(), 503);
        assert_eq!(ErrorCode::MethodNotAllowed.status(), 405);
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        for code in ErrorCode::ALL {
            let transient = matches!(code, ErrorCode::RateLimited | ErrorCode::Unavailable);
            assert_eq!(code.retryable(), transient, "{code:?}");
            assert!(!code.message_vi().is_empty());
        }
    }
}
