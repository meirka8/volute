use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;
use std::fs;

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

pub async fn list() -> Result<()> {
    println!("Fetching available components...");
    // Mocking the logic for now since we don't have a real release history yet.
    // In production, this would hit: https://api.github.com/repos/meirka8/cvc/releases/latest
    println!("Available components (from latest release):");
    println!(" - cvc-mcp (Agent Protocol Server)");
    println!(" - cvc-lsp (Language Server)");
    Ok(())
}

pub async fn install(name: &str) -> Result<()> {
    println!("Installing component: {}", name);

    // 1. Determine Install Path
    let proj_dirs = ProjectDirs::from("dev", "volute", "cvc")
        .context("Could not determine config directory")?;
    let bin_dir = proj_dirs.data_local_dir().join("bin");
    fs::create_dir_all(&bin_dir)?;

    let target_arch = get_target_arch();
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    let asset_name = format!("{}-{}.{}", name, target_arch, extension);

    println!("Looking for asset: {}", asset_name);
    // Placeholder URL - in real logic we'd fetch the release JSON and find the asset url
    let download_url = format!(
        "https://github.com/meirka8/cvc/releases/latest/download/{}",
        asset_name
    );

    // Simulating download/install since the URL doesn't exist yet
    println!("Downloading from {}...", download_url);

    // Logic to actually download would go here:
    // let client = reqwest::Client::new();
    // let response = client.get(&download_url).header(USER_AGENT, "cvc-cli").send().await?;
    // let bytes = response.bytes().await?;
    // extract_tar_gz(&bytes, &bin_dir)?;

    println!("Simulated installation of {} to {:?}", name, bin_dir);
    println!(
        "Make sure {:?} is in your PATH (or configure your editor to point here).",
        bin_dir
    );

    Ok(())
}

pub async fn update(name: &str) -> Result<()> {
    println!("Updating component: {}", name);
    install(name).await
}

fn get_target_arch() -> String {
    // Simplistic arch detection logic
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let os_tag = match os {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        _ => "unknown",
    };

    format!("{}-{}", arch, os_tag)
}

// fn extract_tar_gz(bytes: &[u8], target_dir: &Path) -> Result<()> {
//     let tar = flate2::read::GzDecoder::new(bytes);
//     let mut archive = tar::Archive::new(tar);
//     archive.unpack(target_dir)?;
//     Ok(())
// }
