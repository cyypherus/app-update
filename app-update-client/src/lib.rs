use anyhow::Result;
use app_update::{AvailableUpdate, Platform, UpdateApi};
use reqwest::{Client, Response};
use semver::Version;
use serde::Deserialize;
use std::collections::HashMap;
use thiserror::Error;

pub use app_update_models::*;

#[derive(Error, Debug)]
pub enum VersionServerError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("Authentication failed")]
    Authentication,

    #[error("Version not found")]
    VersionNotFound,

    #[error("Platform not supported: {0}")]
    UnsupportedPlatform(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("invalid version `{version}`: {source}")]
    InvalidVersion {
        version: String,
        #[source]
        source: semver::Error,
    },

    #[error("missing SHA-256 for {version} on {platform}")]
    MissingChecksum { version: String, platform: String },

    #[error("invalid SHA-256 for {version} on {platform}: {source}")]
    InvalidChecksum {
        version: Version,
        platform: String,
        #[source]
        source: app_update::ChecksumError,
    },
}

#[derive(Debug, Clone)]
pub struct VersionServerClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl VersionServerClient {
    pub fn new<S: Into<String>>(base_url: S) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: None,
        }
    }

    pub fn with_api_key<S: Into<String>>(mut self, api_key: S) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn for_app<S: Into<String>>(self, app_name: S) -> VersionServerAppClient {
        VersionServerAppClient {
            client: self,
            app_name: app_name.into(),
        }
    }

    async fn handle_response<T>(&self, response: Response) -> Result<T, VersionServerError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let status = response.status();

        if status.is_success() {
            Ok(response.json().await?)
        } else {
            let error_text = response.text().await.unwrap_or_default();

            match status.as_u16() {
                401 => Err(VersionServerError::Authentication),
                404 => Err(VersionServerError::VersionNotFound),
                _ => {
                    let message = if let Ok(error_response) =
                        serde_json::from_str::<ErrorResponse>(&error_text)
                    {
                        error_response.error
                    } else {
                        error_text
                    };
                    Err(VersionServerError::Api {
                        status: status.as_u16(),
                        message,
                    })
                }
            }
        }
    }

    fn add_auth_header(&self, request_builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(api_key) = &self.api_key {
            request_builder.header("Authorization", format!("Bearer {api_key}"))
        } else {
            request_builder
        }
    }

    pub async fn list_versions<S: AsRef<str>>(
        &self,
        app_name: S,
    ) -> Result<Vec<VersionResponse>, VersionServerError> {
        let app_name = app_name.as_ref();
        let response = self
            .client
            .get(format!("{}/{}/versions", self.base_url, app_name))
            .send()
            .await?;

        match response.status().as_u16() {
            200 => {
                let json: serde_json::Value = response.json().await?;
                if let Some(versions) = json.get("versions") {
                    Ok(serde_json::from_value(versions.clone())?)
                } else {
                    Ok(Vec::new())
                }
            }
            404 => Ok(Vec::new()),
            _ => Err(self
                .handle_response::<Vec<VersionResponse>>(response)
                .await
                .unwrap_err()),
        }
    }

    pub async fn get_latest_version<S1: AsRef<str>, S2: AsRef<str>>(
        &self,
        app_name: S1,
        platform: S2,
    ) -> Result<Option<VersionResponse>, VersionServerError> {
        let platform = platform.as_ref();

        if !SUPPORTED_PLATFORMS.contains(&platform) {
            return Err(VersionServerError::UnsupportedPlatform(
                platform.to_string(),
            ));
        }

        let versions = self.list_versions(app_name).await?;
        Ok(versions
            .into_iter()
            .find(|version| version.platforms.contains(&platform.to_string())))
    }

    pub async fn download_version<S1: AsRef<str>, S2: AsRef<str>, S3: AsRef<str>>(
        &self,
        app_name: S1,
        platform: S2,
        version: S3,
    ) -> Result<Vec<u8>, VersionServerError> {
        let app_name = app_name.as_ref();
        let platform = platform.as_ref();
        let version = version.as_ref();

        if !SUPPORTED_PLATFORMS.contains(&platform) {
            return Err(VersionServerError::UnsupportedPlatform(
                platform.to_string(),
            ));
        }

        let response = self
            .client
            .get(format!(
                "{}/{}/download/{}/{}",
                self.base_url, app_name, platform, version
            ))
            .send()
            .await?;

        match response.status().as_u16() {
            200 => Ok(response.bytes().await?.to_vec()),
            404 => Err(VersionServerError::VersionNotFound),
            400 => Err(VersionServerError::UnsupportedPlatform(
                platform.to_string(),
            )),
            _ => Err(self.handle_response::<()>(response).await.unwrap_err()),
        }
    }

    pub async fn upload_version<S1: AsRef<str>, S2: AsRef<str>>(
        &self,
        app_name: S1,
        version: S2,
        files: &HashMap<String, Vec<u8>>,
    ) -> Result<UploadResponse, VersionServerError> {
        const CHUNK_SIZE: usize = 50 * 1024 * 1024; // 50MB chunks

        let app_name = app_name.as_ref();
        let version = version.as_ref();

        // Always use multipart upload

        for (platform, file_content) in files {
            if !SUPPORTED_PLATFORMS.contains(&platform.as_str()) {
                return Err(VersionServerError::UnsupportedPlatform(platform.clone()));
            }

            // Calculate SHA256 hash
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(file_content);
            let hash = format!("{:x}", hasher.finalize());

            // Create multipart upload
            let create_response = self
                .add_auth_header(
                    self.client
                        .post(format!("{}/{}/upload", self.base_url, app_name))
                        .query(&[
                            ("action", "mpu-create"),
                            ("version", version),
                            ("platform", platform),
                        ]),
                )
                .send()
                .await?;

            let create_result: MultipartCreateResponse =
                self.handle_response(create_response).await?;
            let upload_id = &create_result.upload_id;

            // Upload parts
            let chunks: Vec<&[u8]> = file_content.chunks(CHUNK_SIZE).collect();
            let mut parts = Vec::new();

            for (part_number, chunk) in chunks.iter().enumerate() {
                let part_num = (part_number + 1) as u16;

                let upload_response = self
                    .add_auth_header(
                        self.client
                            .put(format!("{}/{}/upload", self.base_url, app_name))
                            .query(&[
                                ("action", "mpu-uploadpart"),
                                ("uploadId", upload_id),
                                ("partNumber", &part_num.to_string()),
                                ("version", version),
                                ("platform", platform),
                            ])
                            .body(chunk.to_vec()),
                    )
                    .send()
                    .await?;

                let part_result: MultipartPartResponse =
                    self.handle_response(upload_response).await?;
                parts.push(serde_json::json!({
                    "partNumber": part_result.part_number,
                    "etag": part_result.etag
                }));
            }

            // Complete multipart upload
            let complete_response = self
                .add_auth_header(
                    self.client
                        .post(format!("{}/{}/upload", self.base_url, app_name))
                        .query(&[
                            ("action", "mpu-complete"),
                            ("uploadId", upload_id),
                            ("version", version),
                            ("platform", platform),
                        ])
                        .json(&serde_json::json!({"parts": parts})),
                )
                .send()
                .await?;

            let _complete_result: MultipartCompleteResponse =
                self.handle_response(complete_response).await?;

            // Register the completed upload
            let register_response = self
                .add_auth_header(
                    self.client
                        .post(format!("{}/{}/upload/finish", self.base_url, app_name))
                        .json(&CompleteVersionRequest {
                            version: version.to_string(),
                            platform: platform.clone(),
                            sha256: hash,
                        }),
                )
                .send()
                .await?;

            let register_result: CompleteVersionResponse =
                self.handle_response(register_response).await?;

            if !register_result.success {
                return Err(VersionServerError::Api {
                    status: 500,
                    message: format!("Failed to register version: {}", register_result.message),
                });
            }
        }

        Ok(UploadResponse {
            success: true,
            message: "Version uploaded successfully".to_string(),
            app_name: app_name.to_string(),
            version: version.to_string(),
            platforms: files.keys().cloned().collect(),
        })
    }

    pub async fn delete_version<S1: AsRef<str>, S2: AsRef<str>>(
        &self,
        app_name: S1,
        version: S2,
    ) -> Result<DeleteResponse, VersionServerError> {
        let app_name = app_name.as_ref();
        let version = version.as_ref();
        let request = self
            .client
            .delete(format!("{}/{}/delete/{}", self.base_url, app_name, version));

        let response = self.add_auth_header(request).send().await?;

        match response.status().as_u16() {
            200 => Ok(self.handle_response(response).await?),
            404 => Err(VersionServerError::VersionNotFound),
            _ => Err(self
                .handle_response::<DeleteResponse>(response)
                .await
                .unwrap_err()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VersionServerAppClient {
    client: VersionServerClient,
    app_name: String,
}

impl UpdateApi for VersionServerAppClient {
    type Error = VersionServerError;

    async fn latest_update(
        &self,
        platform: Platform,
    ) -> Result<Option<AvailableUpdate>, Self::Error> {
        let platform_name = platform.as_str();
        let Some(latest) = self
            .client
            .get_latest_version(&self.app_name, platform_name)
            .await?
        else {
            return Ok(None);
        };

        let version = Version::parse(&latest.version).map_err(|source| {
            VersionServerError::InvalidVersion {
                version: latest.version.clone(),
                source,
            }
        })?;
        let sha256 = latest.sha256s.get(platform_name).ok_or_else(|| {
            VersionServerError::MissingChecksum {
                version: latest.version.clone(),
                platform: platform_name.to_string(),
            }
        })?;
        let update = AvailableUpdate::new(version.clone(), sha256).map_err(|source| {
            VersionServerError::InvalidChecksum {
                version,
                platform: platform_name.to_string(),
                source,
            }
        })?;

        Ok(Some(update))
    }

    async fn download_update(
        &self,
        update: &AvailableUpdate,
        platform: Platform,
    ) -> Result<Vec<u8>, Self::Error> {
        self.client
            .download_version(
                &self.app_name,
                platform.as_str(),
                update.version.to_string(),
            )
            .await
    }
}
