use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const SUPPORTED_PLATFORMS: &[&str] = &["macos-arm", "macos-intel", "windows-x86_64-gnu"];

pub const VERSION_SERVER_PROD: &str = "https://apps.cyypher.com";
pub const VERSION_SERVER_DEV: &str = "https://dev.cyypher.com";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VersionResponse {
    pub app_name: String,
    pub version: String,
    pub timestamp: String,
    pub platforms: Vec<String>,
    pub sha256s: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LatestVersionResponse {
    pub app_name: String,
    pub platform: String,
    pub version: String,
    pub timestamp: String,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UploadResponse {
    pub success: bool,
    pub message: String,
    pub app_name: String,
    pub version: String,
    pub platforms: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeleteResponse {
    pub success: bool,
    pub message: String,
    pub app_name: String,
    pub version: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AppVersion {
    pub id: Option<i64>,
    pub app_name: String,
    pub version: String,
    pub platform: String,
    pub timestamp: String,
    pub sha256: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MultipartCreateResponse {
    pub key: String,
    #[serde(rename = "uploadId")]
    pub upload_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MultipartPartResponse {
    #[serde(rename = "partNumber")]
    pub part_number: u16,
    pub etag: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MultipartCompleteResponse {
    pub success: bool,
    pub etag: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CompleteVersionRequest {
    pub version: String,
    pub platform: String,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CompleteVersionResponse {
    pub success: bool,
    pub message: String,
    pub app_name: String,
    pub version: String,
    pub platform: String,
}
