use app_update_models::*;
use worker::*;
const DB_NAME: &str = "version-server-d1";
const BUCKET_NAME: &str = "version-server-r2";

macro_rules! try_or_500 {
    ($expr:expr, $msg:literal) => {
        match $expr {
            Ok(val) => val,
            Err(e) => {
                return Ok(Response::from_json(&ErrorResponse {
                    error: format!("Internal server error: {}: {}", $msg, e),
                })
                .unwrap()
                .with_status(500));
            }
        }
    };
}

fn make_error_response(status: u16, message: String) -> Response {
    Response::from_json(&ErrorResponse { error: message })
        .unwrap()
        .with_status(status)
}

fn bad_request(message: impl Into<String>) -> Result<Response> {
    Response::from_json(&ErrorResponse {
        error: message.into(),
    })
    .map(|r| r.with_status(400))
}

fn not_found(message: impl Into<String>) -> Result<Response> {
    Response::from_json(&ErrorResponse {
        error: message.into(),
    })
    .map(|r| r.with_status(404))
}

fn unauthorized(message: impl Into<String>) -> Response {
    Response::from_json(&ErrorResponse {
        error: message.into(),
    })
    .unwrap()
    .with_status(401)
}

fn internal_error(message: impl Into<String>) -> Response {
    Response::from_json(&ErrorResponse {
        error: message.into(),
    })
    .unwrap()
    .with_status(500)
}

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    let router = Router::new();

    router
        .post_async("/:app/upload", handle_multipart_post)
        .put_async("/:app/upload", handle_multipart_put)
        .delete_async("/:app/upload", handle_multipart_delete)
        .post_async("/:app/upload/finish", complete_version_upload)
        .get_async("/:app/versions", list_versions)
        .get_async("/:app/download/:platform/:version", download_version)
        .get_async("/:app/download/:platform/latest", download_latest_version)
        .delete_async("/:app/delete/:version", delete_version)
        .run(req, env)
        .await
}

async fn handle_multipart_post(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(response) = authenticate_request(&req, &ctx.env).await {
        return Ok(response);
    }

    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let url = req.url()?;
    let action = match url
        .query_pairs()
        .find(|(key, _)| key == "action")
        .map(|(_, value)| value.to_string())
    {
        Some(a) => a,
        None => return bad_request("Action parameter is required"),
    };

    match action.as_str() {
        "mpu-create" => {
            let version = match url
                .query_pairs()
                .find(|(key, _)| key == "version")
                .map(|(_, value)| value.to_string())
            {
                Some(v) => v,
                None => return bad_request("version parameter is required"),
            };

            let platform = match url
                .query_pairs()
                .find(|(key, _)| key == "platform")
                .map(|(_, value)| value.to_string())
            {
                Some(p) => p,
                None => return bad_request("platform parameter is required"),
            };

            let key = format!("{app_name}/{version}/{app_name}-{platform}.zip");

            let bucket = try_or_500!(ctx.env.bucket(BUCKET_NAME), "Failed to get bucket");
            let multipart_upload = try_or_500!(
                bucket.create_multipart_upload(&key).execute().await,
                "Failed to create multipart upload"
            );

            let upload_id = multipart_upload.upload_id().await;
            Response::from_json(&MultipartCreateResponse {
                key: key.to_string(),
                upload_id,
            })
        }
        "mpu-complete" => {
            let version = match url
                .query_pairs()
                .find(|(key, _)| key == "version")
                .map(|(_, value)| value.to_string())
            {
                Some(v) => v,
                None => return bad_request("version parameter is required"),
            };

            let platform = match url
                .query_pairs()
                .find(|(key, _)| key == "platform")
                .map(|(_, value)| value.to_string())
            {
                Some(p) => p,
                None => return bad_request("platform parameter is required"),
            };

            let upload_id = match url
                .query_pairs()
                .find(|(key, _)| key == "uploadId")
                .map(|(_, value)| value.to_string())
            {
                Some(id) => id,
                None => return bad_request("uploadId parameter is required"),
            };

            let key = format!("{app_name}/{version}/{app_name}-{platform}.zip");

            #[derive(serde::Deserialize)]
            struct CompleteBody {
                parts: Vec<serde_json::Value>,
            }

            let body: CompleteBody = try_or_500!(req.json().await, "Failed to parse request body");

            let bucket = try_or_500!(ctx.env.bucket(BUCKET_NAME), "Failed to get bucket");
            let multipart_upload = try_or_500!(
                bucket.resume_multipart_upload(&key, &upload_id),
                "Failed to resume multipart upload"
            );

            let parts: Vec<worker::UploadedPart> = body
                .parts
                .into_iter()
                .filter_map(|part| {
                    let part_number = part.get("partNumber")?.as_u64()? as u16;
                    let etag = part.get("etag")?.as_str()?.to_string();
                    Some(worker::UploadedPart::new(part_number, etag))
                })
                .collect();

            let object = try_or_500!(
                multipart_upload.complete(parts).await,
                "Failed to complete multipart upload"
            );

            Response::from_json(&MultipartCompleteResponse {
                success: true,
                etag: object.http_etag(),
            })
        }
        _ => bad_request(format!("Unknown action {action} for POST")).map(|r| r.with_status(400)),
    }
}

async fn handle_multipart_put(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(response) = authenticate_request(&req, &ctx.env).await {
        return Ok(response);
    }

    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let url = req.url()?;
    let action = match url
        .query_pairs()
        .find(|(key, _)| key == "action")
        .map(|(_, value)| value.to_string())
    {
        Some(a) => a,
        None => return bad_request("Action parameter is required"),
    };

    match action.as_str() {
        "mpu-uploadpart" => {
            let version = match url
                .query_pairs()
                .find(|(key, _)| key == "version")
                .map(|(_, value)| value.to_string())
            {
                Some(v) => v,
                None => return bad_request("version parameter is required"),
            };

            let platform = match url
                .query_pairs()
                .find(|(key, _)| key == "platform")
                .map(|(_, value)| value.to_string())
            {
                Some(p) => p,
                None => return bad_request("platform parameter is required"),
            };

            let upload_id = match url
                .query_pairs()
                .find(|(key, _)| key == "uploadId")
                .map(|(_, value)| value.to_string())
            {
                Some(id) => id,
                None => return bad_request("uploadId parameter is required"),
            };

            let part_number = match url
                .query_pairs()
                .find(|(key, _)| key == "partNumber")
                .and_then(|(_, value)| value.parse::<u16>().ok())
            {
                Some(num) => num,
                None => return bad_request("partNumber parameter is required"),
            };

            let key = format!("{app_name}/{version}/{app_name}-{platform}.zip");
            let body_bytes = try_or_500!(req.bytes().await, "Failed to read request body");

            let bucket = try_or_500!(ctx.env.bucket(BUCKET_NAME), "Failed to get bucket");
            let multipart_upload = try_or_500!(
                bucket.resume_multipart_upload(&key, &upload_id),
                "Failed to resume multipart upload"
            );

            let uploaded_part = try_or_500!(
                multipart_upload.upload_part(part_number, body_bytes).await,
                "Failed to upload part"
            );

            Response::from_json(&MultipartPartResponse {
                part_number: uploaded_part.part_number(),
                etag: uploaded_part.etag(),
            })
        }
        _ => bad_request(format!("Unknown action {action} for PUT")).map(|r| r.with_status(400)),
    }
}

async fn handle_multipart_delete(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(response) = authenticate_request(&_req, &ctx.env).await {
        return Ok(response);
    }

    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let url = _req.url()?;
    let action = match url
        .query_pairs()
        .find(|(key, _)| key == "action")
        .map(|(_, value)| value.to_string())
    {
        Some(a) => a,
        None => return bad_request("Action parameter is required"),
    };

    match action.as_str() {
        "mpu-abort" => {
            let version = match url
                .query_pairs()
                .find(|(key, _)| key == "version")
                .map(|(_, value)| value.to_string())
            {
                Some(v) => v,
                None => return bad_request("version parameter is required"),
            };

            let platform = match url
                .query_pairs()
                .find(|(key, _)| key == "platform")
                .map(|(_, value)| value.to_string())
            {
                Some(p) => p,
                None => return bad_request("platform parameter is required"),
            };

            let upload_id = match url
                .query_pairs()
                .find(|(key, _)| key == "uploadId")
                .map(|(_, value)| value.to_string())
            {
                Some(id) => id,
                None => return bad_request("uploadId parameter is required"),
            };

            let key = format!("{app_name}/{version}/{app_name}-{platform}.zip");

            let bucket = try_or_500!(ctx.env.bucket(BUCKET_NAME), "Failed to get bucket");
            let multipart_upload = try_or_500!(
                bucket.resume_multipart_upload(&key, &upload_id),
                "Failed to resume multipart upload"
            );

            try_or_500!(
                multipart_upload.abort().await,
                "Failed to abort multipart upload"
            );

            Ok(Response::empty()?.with_status(204))
        }
        _ => bad_request(format!("Unknown action {action} for DELETE")).map(|r| r.with_status(400)),
    }
}

async fn complete_version_upload(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(response) = authenticate_request(&req, &ctx.env).await {
        return Ok(response);
    }

    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let request: CompleteVersionRequest =
        try_or_500!(req.json().await, "Failed to parse request body");

    let db = try_or_500!(ctx.env.d1(DB_NAME), "Failed to get database");
    let timestamp = chrono::Utc::now().to_rfc3339();

    let stmt = try_or_500!(db
        .prepare("INSERT OR REPLACE INTO app_versions (app_name, version, platform, timestamp, sha256) VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(&[
            app_name.into(),
            request.version.clone().into(),
            request.platform.clone().into(),
            timestamp.into(),
            request.sha256.into(),
        ]), "Failed to prepare database statement");

    try_or_500!(stmt.run().await, "Failed to execute database query");

    Response::from_json(&CompleteVersionResponse {
        success: true,
        message: "Version upload completed successfully".to_string(),
        app_name: app_name.to_string(),
        version: request.version,
        platform: request.platform,
    })
}

async fn get_versions_for_app(
    app_name: &str,
    ctx: &RouteContext<()>,
) -> std::result::Result<Vec<VersionResponse>, Response> {
    let db = ctx
        .env
        .d1(DB_NAME)
        .map_err(|e| make_error_response(500, format!("Failed to get database: {e}")))?;

    let stmt = db
        .prepare("SELECT version, timestamp, platform, sha256, created_at FROM app_versions WHERE app_name = ?1 ORDER BY created_at DESC")
        .bind(&[app_name.into()])
        .map_err(|e| make_error_response(500, format!("Failed to prepare database statement: {e}")))?;

    let result = stmt
        .all()
        .await
        .map_err(|e| make_error_response(500, format!("Failed to execute database query: {e}")))?;

    let mut version_map: std::collections::HashMap<String, VersionResponse> =
        std::collections::HashMap::new();

    for row in result.results::<serde_json::Value>().unwrap_or_default() {
        let version = row["version"].as_str().unwrap_or("").to_string();
        let timestamp = row["timestamp"].as_str().unwrap_or("").to_string();
        let platform = row["platform"].as_str().unwrap_or("").to_string();
        let sha256 = row["sha256"].as_str().unwrap_or("").to_string();

        let key = version.clone();

        let version_response = version_map.entry(key).or_insert_with(|| VersionResponse {
            app_name: app_name.to_string(),
            version: version.clone(),
            timestamp: timestamp.clone(),
            platforms: Vec::new(),
            sha256s: std::collections::HashMap::new(),
        });

        if timestamp > version_response.timestamp {
            version_response.timestamp = timestamp;
        }

        version_response.platforms.push(platform.clone());
        version_response.sha256s.insert(platform, sha256);
    }

    let mut versions: Vec<VersionResponse> = version_map.into_values().collect();
    versions.sort_by(|a, b| {
        match (
            semver::Version::parse(&a.version),
            semver::Version::parse(&b.version),
        ) {
            (Ok(ver_a), Ok(ver_b)) => ver_b.cmp(&ver_a),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => b.timestamp.cmp(&a.timestamp),
        }
    });

    Ok(versions)
}

async fn download_file_from_bucket(
    app_name: &str,
    version: &str,
    platform: &str,
    ctx: &RouteContext<()>,
) -> std::result::Result<Response, Response> {
    let bucket = ctx
        .env
        .bucket(BUCKET_NAME)
        .map_err(|e| make_error_response(500, format!("Failed to get bucket: {e}")))?;
    let file_key = format!("{app_name}/{version}/{app_name}-{platform}.zip");

    let file_obj = bucket
        .get(&file_key)
        .execute()
        .await
        .map_err(|e| make_error_response(500, format!("Failed to get file from bucket: {e}")))?
        .ok_or_else(|| make_error_response(404, "File not found".to_string()))?;

    let filename = format!("{app_name}-{platform}-{version}.zip");
    let headers = Headers::new();
    headers
        .set("Content-Type", "application/zip")
        .map_err(|e| make_error_response(500, format!("Failed to set headers: {e}")))?;
    headers
        .set(
            "Content-Disposition",
            &format!("attachment; filename=\"{filename}\""),
        )
        .map_err(|e| make_error_response(500, format!("Failed to set headers: {e}")))?;
    headers
        .set("Cache-Control", "public, max-age=3600")
        .map_err(|e| make_error_response(500, format!("Failed to set headers: {e}")))?;
    headers
        .set("Content-Length", &file_obj.size().to_string())
        .map_err(|e| make_error_response(500, format!("Failed to set headers: {e}")))?;

    let body = file_obj
        .body()
        .ok_or_else(|| make_error_response(500, "Failed to get file body stream".to_string()))?;

    let stream = body
        .stream()
        .map_err(|e| make_error_response(500, format!("Failed to get file stream: {e}")))?;

    let response = Response::from_stream(stream)
        .map_err(|e| make_error_response(500, format!("Failed to create response: {e}")))?;

    Ok(response.with_headers(headers))
}

async fn list_versions(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let versions = match get_versions_for_app(app_name, &ctx).await {
        Ok(v) => v,
        Err(response) => return Ok(response),
    };

    Response::from_json(&serde_json::json!({
        "app_name": app_name,
        "versions": versions
    }))
}

async fn download_version(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let platform = match ctx.param("platform") {
        Some(p) => p,
        None => return bad_request("Platform parameter is required"),
    };

    let version = match ctx.param("version") {
        Some(v) => v,
        None => return bad_request("Version parameter is required"),
    };

    if !SUPPORTED_PLATFORMS.contains(&platform.as_str()) {
        return bad_request(format!("Unsupported platform: {platform}"));
    }

    let db = try_or_500!(ctx.env.d1(DB_NAME), "Failed to get database");

    let stmt = try_or_500!(db
        .prepare("SELECT app_name, version, timestamp, platform, sha256 FROM app_versions WHERE app_name = ?1 AND version = ?2 AND platform = ?3")
        .bind(&[app_name.into(), version.into(), platform.into()]), "Failed to prepare database statement");

    let result = try_or_500!(
        stmt.first::<AppVersion>(None).await,
        "Failed to execute database query"
    );

    let _app_version = match result {
        Some(v) => v,
        None => return not_found("Version not found for platform"),
    };

    match download_file_from_bucket(app_name, version, platform, &ctx).await {
        Ok(response) => Ok(response),
        Err(response) => Ok(response),
    }
}

async fn delete_version(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(response) = authenticate_request(&req, &ctx.env).await {
        return Ok(response);
    }

    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let version = match ctx.param("version") {
        Some(v) => v,
        None => return bad_request("Version parameter is required"),
    };

    let db = try_or_500!(ctx.env.d1(DB_NAME), "Failed to get database");
    let bucket = try_or_500!(ctx.env.bucket(BUCKET_NAME), "Failed to get bucket");

    let stmt = try_or_500!(
        db.prepare("SELECT platform FROM app_versions WHERE app_name = ?1 AND version = ?2")
            .bind(&[app_name.into(), version.into()]),
        "Failed to prepare database statement"
    );

    let result = try_or_500!(stmt.all().await, "Failed to execute database query");
    let platforms: Vec<String> = result
        .results::<serde_json::Value>()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row["platform"].as_str().map(|s| s.to_string()))
        .collect();

    if platforms.is_empty() {
        return not_found("Version not found");
    }

    for platform in &platforms {
        let file_key = format!("{app_name}/{version}/{app_name}-{platform}.zip");
        try_or_500!(bucket.delete(&file_key).await, "Failed to delete file");
    }

    let stmt = try_or_500!(
        db.prepare("DELETE FROM app_versions WHERE app_name = ?1 AND version = ?2")
            .bind(&[app_name.into(), version.into()]),
        "Failed to prepare delete statement"
    );

    try_or_500!(stmt.run().await, "Failed to execute delete query");

    Response::from_json(&DeleteResponse {
        success: true,
        message: "Version deleted successfully".to_string(),
        app_name: app_name.to_string(),
        version: version.to_string(),
    })
}

async fn download_latest_version(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let app_name = match ctx.param("app") {
        Some(app) => app,
        None => return bad_request("App name parameter is required"),
    };

    let platform = match ctx.param("platform") {
        Some(p) => p,
        None => return bad_request("Platform parameter is required"),
    };

    if !SUPPORTED_PLATFORMS.contains(&platform.as_str()) {
        return bad_request(format!("Unsupported platform: {platform}"));
    }

    let versions = match get_versions_for_app(app_name, &ctx).await {
        Ok(v) => v,
        Err(response) => return Ok(response),
    };

    let latest_version = match versions
        .into_iter()
        .find(|version| version.platforms.contains(&platform.to_string()))
    {
        Some(v) => v,
        None => return not_found("No version found for platform"),
    };

    match download_file_from_bucket(app_name, &latest_version.version, platform, &ctx).await {
        Ok(response) => Ok(response),
        Err(response) => Ok(response),
    }
}

async fn authenticate_request(req: &Request, env: &Env) -> std::result::Result<(), Response> {
    let api_key = match req
        .headers()
        .get("Authorization")
        .map_err(|_| internal_error("Failed to read headers"))?
    {
        Some(auth_header) => {
            if let Some(key) = auth_header.strip_prefix("Bearer ") {
                key.to_string()
            } else {
                return Err(unauthorized("Invalid authorization header format"));
            }
        }
        None => {
            return Err(unauthorized("Authorization header required"));
        }
    };

    let expected_key = match env.secret("API_KEY") {
        Ok(secret) => secret.to_string(),
        Err(e) => {
            return Err(internal_error(format!("Failed to get API key: {e}")));
        }
    };

    if api_key != expected_key {
        return Err(unauthorized("Invalid API key"));
    }

    Ok(())
}
