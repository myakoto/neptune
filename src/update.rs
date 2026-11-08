//! Автообновление через GitHub Releases (myakoto/neptune).
//!
//! Релизы публикует pipeline.yml: рядом с архивом лежат сам бинарник
//! `neptune-x86_64-pc-windows-msvc.exe` и его контрольная сумма
//! `.exe.sha256`. Проверка идёт через GitHub REST API, скачивание —
//! напрямую с github.com, контрольная сумма сверяется до подмены.
//!
//! Подмена бинарника — self_replace: на Windows текущий exe переименовывается,
//! новый копируется на его место, а переименованный старый удаляется при выходе
//! из процесса. Пользователю ничего скачивать руками не нужно.

use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO_OWNER: &str = "myakoto";
const REPO_NAME: &str = "neptune";
const API_LATEST: &str = "https://api.github.com/repos/myakoto/neptune/releases/latest";
/// Бинарник в релизе — то, что качает апдейтер.
const ASSET_EXE: &str = "neptune-x86_64-pc-windows-msvc.exe";
/// Контрольная сумма бинарника из того же релиза.
const ASSET_SHA256: &str = "neptune-x86_64-pc-windows-msvc.exe.sha256";
const USER_AGENT: &str = concat!("neptune-updater/", env!("CARGO_PKG_VERSION"));
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const SHA256_HEX_LEN: usize = 64;

/// Ассет релиза на GitHub.
#[derive(Debug, Deserialize)]
struct GithubAsset {
    /// Имя файла ассета.
    name: String,
    /// Прямая ссылка на файл.
    browser_download_url: String,
}

/// Ответ `GET /releases/latest`.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    /// Тег релиза, например `v0.2.0`.
    tag_name: String,
    /// Файлы релиза.
    assets: Vec<GithubAsset>,
}

/// Данные о релизе, нужные для обновления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// Версия без префикса `v`.
    pub version: String,
    /// Ссылка на бинарник.
    pub exe_url: String,
    /// Ссылка на файл контрольной суммы.
    pub sha256_url: String,
}

fn client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("не удалось создать HTTP-клиент")
}

/// Разбирает ответ GitHub и достаёт ссылки на бинарник и контрольную сумму.
///
/// # Errors
/// Возвращает ошибку, если ответ не разбирается или в релизе нет нужных файлов.
fn parse_release(json: &str) -> Result<ReleaseInfo> {
    let release: GithubRelease =
        serde_json::from_str(json).context("не удалось разобрать ответ GitHub")?;
    let version = release.tag_name.trim_start_matches('v').to_owned();
    let exe = release
        .assets
        .iter()
        .find(|asset| asset.name == ASSET_EXE)
        .with_context(|| format!("в релизе нет файла {ASSET_EXE}"))?;
    let sha = release
        .assets
        .iter()
        .find(|asset| asset.name == ASSET_SHA256)
        .with_context(|| format!("в релизе нет файла {ASSET_SHA256}"))?;
    Ok(ReleaseInfo {
        version,
        exe_url: exe.browser_download_url.clone(),
        sha256_url: sha.browser_download_url.clone(),
    })
}

/// Свежий стабильный релиз; `None`, если релизов ещё нет.
///
/// # Errors
/// Возвращает ошибку при недоступности GitHub или битом ответе.
pub fn latest_release() -> Result<Option<ReleaseInfo>> {
    let response = client()?
        .get(API_LATEST)
        .send()
        .context("не удалось спросить GitHub о последнем релизе")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = response
        .text()
        .context("не удалось прочитать ответ GitHub")?;
    Ok(Some(parse_release(&body)?))
}

/// Версия свежего релиза, если она новее текущей.
///
/// # Errors
/// Возвращает ошибку при недоступности GitHub или битом ответе.
pub fn check() -> Result<Option<String>> {
    let Some(release) = latest_release()? else {
        return Ok(None);
    };
    Ok(
        version_is_newer(env!("CARGO_PKG_VERSION"), &release.version).then_some(release.version),
    )
}

/// Скачивает свежий релиз, сверяет контрольную сумму и подменяет бинарник.
///
/// Возвращает установленную версию.
///
/// # Errors
/// Возвращает ошибку сети, несовпадения контрольной суммы или подмены бинарника.
pub fn apply() -> Result<String> {
    let release = latest_release()?.context("на GitHub нет ни одного релиза")?;
    let http = client()?;
    let dir = tempfile::tempdir().context("не удалось создать временный каталог")?;
    let exe_path = dir.path().join("neptune.exe");

    download_to(&http, &release.exe_url, &exe_path)?;
    let expected = download_text(&http, &release.sha256_url)?;
    let expected_hash =
        parse_sha256(&expected).with_context(|| format!("не удалось разобрать {ASSET_SHA256}"))?;
    let actual_hash = sha256_file(&exe_path)?;
    if !actual_hash.eq_ignore_ascii_case(&expected_hash) {
        anyhow::bail!(
            "контрольная сумма не совпала: ждали {expected_hash}, скачали {actual_hash}"
        );
    }

    self_replace::self_replace(&exe_path).context("не удалось подменить бинарник")?;
    Ok(release.version)
}

/// Скачивает файл в указанное место.
///
/// # Errors
/// Возвращает ошибку сети или записи файла.
fn download_to(http: &Client, url: &str, dest: &Path) -> Result<()> {
    let mut response = http
        .get(url)
        .send()
        .with_context(|| format!("не удалось скачать {url}"))?
        .error_for_status()
        .with_context(|| format!("сервер отклонил {url}"))?;
    let mut file = File::create(dest)
        .with_context(|| format!("не удалось создать {}", dest.display()))?;
    std::io::copy(&mut response, &mut file)
        .with_context(|| format!("не удалось записать {}", dest.display()))?;
    Ok(())
}

/// Скачивает файл как текст.
///
/// # Errors
/// Возвращает ошибку сети или чтения тела ответа.
fn download_text(http: &Client, url: &str) -> Result<String> {
    http.get(url)
        .send()
        .with_context(|| format!("не удалось скачать {url}"))?
        .error_for_status()
        .with_context(|| format!("сервер отклонил {url}"))?
        .text()
        .with_context(|| format!("не удалось прочитать {url}"))
}

/// Достаёт hex-хеш из содержимого файла `.sha256`.
///
/// # Errors
/// Возвращает `None`, если хеша в тексте нет.
fn parse_sha256(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| {
            token.len() == SHA256_HEX_LEN && token.chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase)
}

/// Считает sha256 файла в hex-строке.
///
/// # Errors
/// Возвращает ошибку чтения файла.
fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("не удалось открыть {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Сравнивает версии: `true`, если `candidate` новее `current`.
///
/// Поддерживает префикс `v` и prerelease-суффикс (`0.3.0-beta.1`).
#[must_use]
pub fn version_is_newer(current: &str, candidate: &str) -> bool {
    let (current_core, current_pre) = split_version(current);
    let (candidate_core, candidate_pre) = split_version(candidate);
    match compare_core(&candidate_core, &current_core) {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    match (current_pre, candidate_pre) {
        (None, None) => false,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(current_pre), Some(candidate_pre)) => candidate_pre > current_pre,
    }
}

/// Разбирает версию на числовую часть и prerelease-суффикс.
fn split_version(version: &str) -> (Vec<u64>, Option<String>) {
    let trimmed = version.trim().trim_start_matches('v');
    let (core, pre) = match trimmed.split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_owned())),
        None => (trimmed, None),
    };
    let numbers = core
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect();
    (numbers, pre)
}

/// Числа версии сравниваются по компонентам, недостающие считаются нулём.
fn compare_core(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    for index in 0..left.len().max(right.len()) {
        let l = left.get(index).copied().unwrap_or(0);
        let r = right.get(index).copied().unwrap_or(0);
        match l.cmp(&r) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE_JSON: &str = r#"{
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "neptune-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://example/zip"},
            {"name": "neptune-x86_64-pc-windows-msvc.exe", "browser_download_url": "https://example/exe"},
            {"name": "neptune-x86_64-pc-windows-msvc.exe.sha256", "browser_download_url": "https://example/sha"}
        ]
    }"#;

    #[test]
    fn parse_release_finds_exe_and_checksum() {
        let release = parse_release(RELEASE_JSON).expect("релиз должен разобраться");
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.exe_url, "https://example/exe");
        assert_eq!(release.sha256_url, "https://example/sha");
    }

    #[test]
    fn parse_release_requires_checksum_asset() {
        let json = r#"{"tag_name":"v0.2.0","assets":[{"name":"neptune-x86_64-pc-windows-msvc.exe","browser_download_url":"u"}]}"#;
        assert!(parse_release(json).is_err());
    }

    #[test]
    fn newer_version_is_detected() {
        assert!(version_is_newer("0.1.2", "0.2.0"));
        assert!(version_is_newer("0.1.2", "v0.1.3"));
        assert!(version_is_newer("0.1.2", "0.2.0-beta.1"));
        assert!(version_is_newer("0.1.9", "0.1.10"));
    }

    #[test]
    fn same_or_older_version_is_not_newer() {
        assert!(!version_is_newer("0.2.0", "0.2.0"));
        assert!(!version_is_newer("0.2.0", "0.1.9"));
        assert!(!version_is_newer("0.3.0", "0.3.0-beta.1"));
        assert!(!version_is_newer("0.2.0", "0.2.0-beta.2"));
    }

    #[test]
    fn prerelease_of_newer_core_beats_older_core() {
        assert!(version_is_newer("0.2.0", "0.3.0-beta.1"));
    }

    #[test]
    fn sha256_is_taken_from_first_token() {
        let text = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  neptune.exe\n";
        assert_eq!(
            parse_sha256(text),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_owned())
        );
    }

    #[test]
    fn sha256_must_be_hex_and_64_chars() {
        assert!(parse_sha256("не хеш").is_none());
        assert!(parse_sha256("deadbeef").is_none());
    }

    #[test]
    fn sha256_of_file_matches_known_value() {
        let dir = tempfile::tempdir().expect("временный каталог");
        let path = dir.path().join("abc");
        fs::write(&path, b"abc").expect("запись файла");
        assert_eq!(
            sha256_file(&path).expect("хеш файла"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn core_comparison_pads_missing_parts() {
        assert_eq!(compare_core(&[0, 2], &[0, 2, 0]), std::cmp::Ordering::Equal);
        assert_eq!(compare_core(&[0, 3], &[0, 2, 9]), std::cmp::Ordering::Greater);
    }

    #[test]
    fn repo_constants_match_release_assets() {
        assert!(RELEASE_JSON.contains(ASSET_EXE));
        assert!(RELEASE_JSON.contains(ASSET_SHA256));
        assert_eq!(REPO_OWNER, "myakoto");
        assert_eq!(REPO_NAME, "neptune");
    }
}
