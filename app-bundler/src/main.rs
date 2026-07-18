use clap::Parser;
use dotenv::dotenv;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "app-bundler")]
struct Args {
    #[arg(long)]
    project_root: PathBuf,
    #[arg(long)]
    package: String,
    #[arg(long)]
    binary: String,
    #[arg(long)]
    app_name: String,
    #[arg(long)]
    windows_icon: Option<PathBuf>,
    #[arg(long)]
    upload_prod: bool,
    #[arg(long)]
    skip_upload: bool,
    #[arg(long)]
    skip_codesign: bool,
    #[arg(long)]
    skip_build: bool,
    #[arg(long, value_delimiter = ',')]
    platforms: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    dotenv().ok();
    let args = Args::parse();
    let project_root = args.project_root.canonicalize()?;
    let platforms = args.platforms.unwrap_or_else(|| {
        vec![
            "macos-arm".to_string(),
            "macos-intel".to_string(),
            "windows-x86_64-gnu".to_string(),
        ]
    });
    let version = app_version(&project_root, &args.package)?;

    let mut archives = if args.skip_build {
        existing_archives(&project_root, &args.app_name, &platforms)
    } else {
        build_archives(
            &project_root,
            &args.package,
            &args.binary,
            &args.app_name,
            args.windows_icon.as_deref(),
            &platforms,
            args.upload_prod,
        )?
    };

    if !args.skip_codesign
        && platforms
            .iter()
            .any(|platform| platform.starts_with("macos-"))
        && apple_credentials_present()
    {
        sign_macos_archives(&project_root, &args.app_name, &platforms, &mut archives)?;
    }

    if !args.skip_upload && env::var_os("VERSION_SERVER_API_KEY").is_some() {
        upload(&version, &args.app_name, &archives, args.upload_prod)?;
    }

    Ok(())
}

fn app_version(project_root: &Path, package: &str) -> Result<String> {
    let manifest = project_root.join("Cargo.toml");
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()?;
    if !output.status.success() {
        return Err("cargo metadata failed".into());
    }

    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    metadata
        .packages
        .into_iter()
        .find(|item| item.name == package)
        .map(|item| item.version)
        .ok_or_else(|| format!("package not found: {package}").into())
}

fn build_archives(
    project_root: &Path,
    package: &str,
    binary: &str,
    app_name: &str,
    windows_icon: Option<&Path>,
    platforms: &[String],
    prod: bool,
) -> Result<Vec<(String, PathBuf)>> {
    let mut archives = Vec::new();
    for platform in platforms {
        let archive = match platform.as_str() {
            "macos-arm" => {
                let target = "aarch64-apple-darwin";
                cargo_bundle(project_root, package, binary, target, prod)?;
                let bundle = find_bundle(project_root, target)?;
                zip_bundle(
                    &bundle,
                    &project_root.join("target"),
                    &format!("{app_name}-macos-arm.zip"),
                )?
            }
            "macos-intel" => {
                let target = "x86_64-apple-darwin";
                cargo_bundle(project_root, package, binary, target, prod)?;
                let bundle = find_bundle(project_root, target)?;
                zip_bundle(
                    &bundle,
                    &project_root.join("target"),
                    &format!("{app_name}-macos-intel.zip"),
                )?
            }
            "windows-x86_64-gnu" => {
                let resource = windows_icon
                    .map(|icon| prepare_windows_icon_resource(project_root, icon))
                    .transpose()?;
                cargo_windows(project_root, package, binary, resource.as_deref(), prod)?;
                let executable = project_root
                    .join("target/x86_64-pc-windows-gnu/release")
                    .join(format!("{binary}.exe"));
                if !executable.exists() {
                    return Err(
                        format!("Windows executable not found: {}", executable.display()).into(),
                    );
                }
                zip_executable(
                    &executable,
                    &project_root.join("target"),
                    &format!("{app_name}-windows-x86_64-gnu.zip"),
                )?
            }
            _ => return Err(format!("unsupported platform: {platform}").into()),
        };
        archives.push((platform.clone(), archive));
    }
    Ok(archives)
}

fn cargo_bundle(
    project_root: &Path,
    package: &str,
    binary: &str,
    target: &str,
    prod: bool,
) -> Result<()> {
    let mut args = vec![
        "bundle".to_string(),
        "--release".to_string(),
        "--target".to_string(),
        target.to_string(),
        "--package".to_string(),
        package.to_string(),
        "--bin".to_string(),
        binary.to_string(),
    ];
    if prod {
        args.extend(["--features".to_string(), "prod".to_string()]);
    }
    let status = Command::new("cargo")
        .args(args)
        .current_dir(project_root)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo bundle failed for {target}").into())
    }
}

fn cargo_windows(
    project_root: &Path,
    package: &str,
    binary: &str,
    resource: Option<&Path>,
    prod: bool,
) -> Result<()> {
    let mut args = vec![
        "build".to_string(),
        "--target".to_string(),
        "x86_64-pc-windows-gnu".to_string(),
        "--release".to_string(),
        "--package".to_string(),
        package.to_string(),
        "--bin".to_string(),
        binary.to_string(),
    ];
    if prod {
        args.extend(["--features".to_string(), "prod".to_string()]);
    }
    let mut command = Command::new("cargo");
    command.args(args).current_dir(project_root);
    if let Some(resource) = resource {
        command.env(
            "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS",
            format!("-C link-arg={}", resource.display()),
        );
    }
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err("cargo build failed for x86_64-pc-windows-gnu".into())
    }
}

fn find_bundle(project_root: &Path, target: &str) -> Result<PathBuf> {
    let bundle_dir = project_root
        .join("target")
        .join(target)
        .join("release/bundle/osx");
    let mut bundles = fs::read_dir(&bundle_dir)?
        .filter_map(|entry| entry.ok().map(|item| item.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"));
    let bundle = bundles
        .next()
        .ok_or_else(|| format!("no app bundle in {}", bundle_dir.display()))?;
    if bundles.next().is_some() {
        return Err(format!("multiple app bundles in {}", bundle_dir.display()).into());
    }
    Ok(bundle)
}

fn prepare_windows_icon_resource(project_root: &Path, icon: &Path) -> Result<PathBuf> {
    let icon = if icon.is_absolute() {
        icon.to_path_buf()
    } else {
        project_root.join(icon)
    };
    if !icon.exists() {
        return Err(format!("Windows icon not found: {}", icon.display()).into());
    }
    let resource_dir = project_root.join("target/windows-resource");
    fs::create_dir_all(&resource_dir)?;
    let rc_path = resource_dir.join("app.rc");
    let obj_path = resource_dir.join("app-icon.o");
    fs::write(&rc_path, format!("1 ICON \"{}\"\n", escape_rc_path(&icon)))?;
    let status = Command::new("x86_64-w64-mingw32-windres")
        .args(["-O", "coff", "-o"])
        .arg(&obj_path)
        .arg(&rc_path)
        .current_dir(project_root)
        .status()?;
    if status.success() {
        Ok(obj_path)
    } else {
        Err("failed to compile Windows icon resource".into())
    }
}

fn escape_rc_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn zip_bundle(bundle: &Path, output_dir: &Path, name: &str) -> Result<PathBuf> {
    let output = output_dir.join(name);
    if output.exists() {
        fs::remove_file(&output)?;
    }
    let status = Command::new("zip")
        .args(["-r", name])
        .arg(bundle.file_name().ok_or("invalid bundle path")?)
        .current_dir(bundle.parent().ok_or("bundle has no parent")?)
        .status()?;
    if !status.success() {
        return Err(format!("failed to archive {}", bundle.display()).into());
    }
    fs::rename(bundle.parent().unwrap().join(name), &output)?;
    Ok(output)
}

fn zip_executable(executable: &Path, output_dir: &Path, name: &str) -> Result<PathBuf> {
    let output = output_dir.join(name);
    if output.exists() {
        fs::remove_file(&output)?;
    }
    let status = Command::new("zip")
        .args(["-j", name])
        .arg(executable.file_name().ok_or("invalid executable path")?)
        .current_dir(executable.parent().ok_or("executable has no parent")?)
        .status()?;
    if !status.success() {
        return Err(format!("failed to archive {}", executable.display()).into());
    }
    fs::rename(executable.parent().unwrap().join(name), &output)?;
    Ok(output)
}

fn existing_archives(
    project_root: &Path,
    app_name: &str,
    platforms: &[String],
) -> Vec<(String, PathBuf)> {
    platforms
        .iter()
        .filter_map(|platform| {
            let path = project_root
                .join("target")
                .join(format!("{app_name}-{platform}.zip"));
            path.exists().then(|| (platform.clone(), path))
        })
        .collect()
}

fn apple_credentials_present() -> bool {
    ["APPLE_TEAM_ID", "APPLE_ID", "APPLE_APP_SPECIFIC_PASSWORD"]
        .iter()
        .all(|name| env::var_os(name).is_some())
}

fn sign_macos_archives(
    project_root: &Path,
    app_name: &str,
    platforms: &[String],
    archives: &mut [(String, PathBuf)],
) -> Result<()> {
    let identity_output = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()?;
    if !identity_output.status.success() {
        return Err("failed to find code signing identities".into());
    }
    let identity_output = String::from_utf8_lossy(&identity_output.stdout);
    let identity = identity_output
        .lines()
        .find(|line| line.contains("Developer ID Application"))
        .and_then(|line| {
            let start = line.find('"')?;
            let end = line.rfind('"')?;
            (start < end).then(|| line[start + 1..end].to_string())
        })
        .ok_or("no Developer ID Application certificate found")?;
    let team_id = env::var("APPLE_TEAM_ID")?;
    let apple_id = env::var("APPLE_ID")?;
    let app_password = env::var("APPLE_APP_SPECIFIC_PASSWORD")?;

    for (platform, target) in [
        ("macos-arm", "aarch64-apple-darwin"),
        ("macos-intel", "x86_64-apple-darwin"),
    ] {
        if !platforms.iter().any(|item| item == platform) {
            continue;
        }
        let bundle = project_root
            .join("target")
            .join(target)
            .join("release/bundle/osx")
            .join(format!("{app_name}.app"));
        sign_and_notarize(&bundle, &identity, &team_id, &apple_id, &app_password)?;
        let archive = zip_bundle(
            &bundle,
            &project_root.join("target"),
            &format!("{app_name}-{platform}.zip"),
        )?;
        if let Some(entry) = archives.iter_mut().find(|(name, _)| name == platform) {
            entry.1 = archive;
        }
    }
    Ok(())
}

fn sign_and_notarize(
    bundle: &Path,
    identity: &str,
    team_id: &str,
    apple_id: &str,
    app_password: &str,
) -> Result<()> {
    let status = Command::new("codesign")
        .args(["--timestamp", "--options", "runtime", "--sign", identity])
        .arg(bundle)
        .status()?;
    if !status.success() {
        return Err(format!("failed to code sign {}", bundle.display()).into());
    }

    let temp_zip = bundle.with_extension("temp.zip");
    let status = Command::new("zip")
        .args(["-r"])
        .arg(&temp_zip)
        .arg(bundle.file_name().ok_or("invalid bundle path")?)
        .current_dir(bundle.parent().ok_or("bundle has no parent")?)
        .status()?;
    if !status.success() {
        return Err("failed to create notarization archive".into());
    }

    let status = Command::new("xcrun")
        .args([
            "notarytool",
            "submit",
            "--wait",
            "--no-progress",
            "-f",
            "json",
            "--team-id",
            team_id,
            "--apple-id",
            apple_id,
            "--password",
            app_password,
        ])
        .arg(&temp_zip)
        .status()?;
    fs::remove_file(&temp_zip)?;
    if !status.success() {
        return Err("notarization failed".into());
    }

    let status = Command::new("xcrun")
        .args(["stapler", "staple"])
        .arg(bundle)
        .status()?;
    if !status.success() {
        return Err("stapling notarization failed".into());
    }
    Ok(())
}

fn upload(version: &str, app_name: &str, archives: &[(String, PathBuf)], prod: bool) -> Result<()> {
    let app_update_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing app-update root")?;
    let status = Command::new("cargo")
        .args(["build", "--release", "--package", "app-update-cli"])
        .current_dir(app_update_root)
        .status()?;
    if !status.success() {
        return Err("failed to build app-update-cli".into());
    }
    let cli = app_update_root.join("target/release/app-update-cli");
    let mut args = Vec::new();
    if prod {
        args.push("--prod".to_string());
    }
    args.extend([
        "upload".to_string(),
        app_name.to_string(),
        version.to_string(),
    ]);
    for (platform, archive) in archives {
        args.push(format!("{platform}={}", archive.display()));
    }
    let status = Command::new(cli)
        .args(args)
        .current_dir(app_update_root)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err("app-update-cli upload failed".into())
    }
}
