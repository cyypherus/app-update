use anyhow::Result;
use app_update_client::{VersionServerClient, SUPPORTED_PLATFORMS};
use app_update_models::{VERSION_SERVER_DEV, VERSION_SERVER_PROD};
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "app-update-cli")]
#[command(about = "CLI tool for interacting with version server")]
struct Cli {
    #[arg(long, help = "Use production server")]
    prod: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "List all versions for an app")]
    List {
        #[arg(help = "App name")]
        app: String,
    },
    #[command(about = "Get latest version for app and platform")]
    Latest {
        #[arg(help = "App name")]
        app: String,
        #[arg(help = "Platform", value_parser = validate_platform)]
        platform: String,
    },
    #[command(about = "Download a specific version")]
    Download {
        #[arg(help = "App name")]
        app: String,
        #[arg(help = "Platform", value_parser = validate_platform)]
        platform: String,
        #[arg(help = "Version")]
        version: String,
        #[arg(short, long, help = "Output file path")]
        output: Option<PathBuf>,
    },
    #[command(about = "Upload a new version")]
    Upload {
        #[arg(help = "App name")]
        app: String,
        #[arg(help = "Version")]
        version: String,
        #[arg(help = "Path to zip files (format: platform=/path/to/file.zip)", value_parser = parse_file_arg)]
        files: Vec<(String, PathBuf)>,
    },
    #[command(about = "Delete a version")]
    Delete {
        #[arg(help = "App name")]
        app: String,
        #[arg(help = "Version")]
        version: String,
    },
}

fn validate_platform(platform: &str) -> Result<String, String> {
    if SUPPORTED_PLATFORMS.contains(&platform) {
        Ok(platform.to_string())
    } else {
        Err(format!(
            "Unsupported platform. Supported: {}",
            SUPPORTED_PLATFORMS.join(", ")
        ))
    }
}

fn parse_file_arg(arg: &str) -> Result<(String, PathBuf), String> {
    let parts: Vec<&str> = arg.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err("Format should be: platform=/path/to/file.zip".to_string());
    }

    let platform = parts[0];
    let path = PathBuf::from(parts[1]);

    if !SUPPORTED_PLATFORMS.contains(&platform) {
        return Err(format!(
            "Unsupported platform '{}'. Supported: {}",
            platform,
            SUPPORTED_PLATFORMS.join(", ")
        ));
    }

    if !path.exists() {
        return Err(format!("File does not exist: {}", path.display()));
    }

    Ok((platform.to_string(), path))
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();

    let url = if cli.prod {
        VERSION_SERVER_PROD
    } else {
        VERSION_SERVER_DEV
    };

    let mut client = VersionServerClient::new(url);
    if let Ok(api_key) = env::var("VERSION_SERVER_API_KEY") {
        client = client.with_api_key(api_key);
    }

    match cli.command {
        Commands::List { app } => {
            let versions = client.list_versions(&app).await?;
            if versions.is_empty() {
                println!("No versions found for app '{app}'");
            } else {
                println!("Versions for app '{app}':");
                for version in versions {
                    println!("  {} ({}):", version.version, version.timestamp);
                    for platform in &version.platforms {
                        let unknown_hash = "unknown".to_string();
                        let sha256 = version.sha256s.get(platform).unwrap_or(&unknown_hash);
                        println!("    {platform}: {sha256}");
                    }
                }
            }
        }

        Commands::Latest { app, platform } => {
            match client.get_latest_version(&app, &platform).await? {
                Some(latest) => {
                    println!(
                        "Latest version for {}/{}: {} ({})",
                        app, platform, latest.version, latest.timestamp
                    );
                    if let Some(sha256) = latest.sha256s.get(&platform) {
                        println!("  SHA256: {sha256}");
                    }
                }
                None => {
                    println!("No versions found for app '{app}' on platform '{platform}'");
                }
            }
        }

        Commands::Download {
            app,
            platform,
            version,
            output,
        } => {
            let data = client.download_version(&app, &platform, &version).await?;

            let output_path =
                output.unwrap_or_else(|| PathBuf::from(format!("{app}-{platform}-{version}.zip")));

            fs::write(&output_path, data)?;
            println!(
                "Downloaded {}/{}/{} to {}",
                app,
                platform,
                version,
                output_path.display()
            );
        }

        Commands::Upload {
            app,
            version,
            files,
        } => {
            let mut file_data = HashMap::new();

            for (platform, path) in files {
                let data = fs::read(&path)?;
                file_data.insert(platform, data);
            }

            let response = client.upload_version(&app, &version, &file_data).await?;

            if response.success {
                println!(
                    "Successfully uploaded {} v{} for platforms: [{}]",
                    response.app_name,
                    response.version,
                    response.platforms.join(", ")
                );
            } else {
                println!("Upload failed: {}", response.message);
            }
        }

        Commands::Delete { app, version } => {
            let response = client.delete_version(&app, &version).await?;

            if response.success {
                println!(
                    "Successfully deleted {} v{}",
                    response.app_name, response.version
                );
            } else {
                println!("Delete failed: {}", response.message);
            }
        }
    }

    Ok(())
}
