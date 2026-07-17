-- Migration: 0001_initial.sql
-- Create app_versions table to store version metadata for multiple applications
-- Each platform gets its own record instead of storing platforms as JSON array

CREATE TABLE IF NOT EXISTS app_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    app_name TEXT NOT NULL,
    version TEXT NOT NULL,
    platform TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(app_name, version, platform)
);

CREATE INDEX idx_app_versions_app_name ON app_versions(app_name);
CREATE INDEX idx_app_versions_created_at ON app_versions(created_at);
CREATE INDEX idx_app_versions_app_version ON app_versions(app_name, version);
CREATE INDEX idx_app_versions_platform ON app_versions(platform);
CREATE INDEX idx_app_versions_app_platform ON app_versions(app_name, platform);
CREATE INDEX idx_app_versions_sha256 ON app_versions(sha256);

-- Trigger to update the updated_at timestamp
CREATE TRIGGER update_app_versions_timestamp
    AFTER UPDATE ON app_versions
    FOR EACH ROW
BEGIN
    UPDATE app_versions
    SET updated_at = CURRENT_TIMESTAMP
    WHERE id = NEW.id;
END;


