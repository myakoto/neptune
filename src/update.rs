//! Автообновление через GitHub Releases (myakoto/neptune).
//!
//! Релизы публикует pipeline.yml: рядом с архивом лежат сам бинарник
//! `neptune-x86_64-pc-windows-msvc.exe` и его контрольная сумма
//! `.exe.sha256`. Проверка идёт через GitHub REST API, скачивание —
//! напрямую с github.com, контрольная сумма сверяется до подмены.
//!
//! Подмена бинарника — `self_replace`: на Windows текущий exe переименовывается,
//! новый копируется на его место, а переименованный старый удаляется при выходе
//! из процесса. Пользователю ничего скачивать руками не нужно.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO_OWNER: &str = "myakoto";
const REPO_NAME: &str = "neptune";
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

/// Кэшированная информация о релизе (JSON + таймстамп) для fallback при rate-limit.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ReleaseCache {
    /// Версия без префикса `v`.
    version: String,
    /// URL бинарника.
    exe_url: String,
    /// URL контрольной суммы.
    sha256_url: String,
    /// Unix-timestamp момента получения.
    fetched_at: u64,
}

impl ReleaseCache {
    const TTL_SECS: u64 = 3_600; // 1 час

    fn is_fresh(&self) -> bool {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().saturating_sub(self.fetched_at) < Self::TTL_SECS)
            .unwrap_or(false)
    }
}

fn client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("не удалось создать HTTP-клиент")
}

/// Адрес последнего стабильного релиза.
fn api_latest() -> String {
    format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest")
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

/// Путь к файлу кэша релиза (рядом с exe, но с другим именем).
pub fn cache_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join("update-cache.json")))
        .unwrap_or_else(|| std::env::temp_dir().join("neptune-update-cache.json"))
}

/// Загрузить кэш с диска.
fn load_cache() -> Option<ReleaseCache> {
    std::fs::read_to_string(cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|c| c.is_fresh())
}

/// Сохранить кэш на диск.
fn save_cache(release: &ReleaseInfo) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cache = ReleaseCache {
        version: release.version.clone(),
        exe_url: release.exe_url.clone(),
        sha256_url: release.sha256_url.clone(),
        fetched_at: now,
    };
    if let Ok(json) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(cache_path(), json);
    }
}

/// Альтернативный endpoint: atom feed — не ограничивается rate-limit как API.
fn latest_release_rss() -> Result<Option<ReleaseInfo>> {
    let http = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp = http
        .get(&format!(
            "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/latest.atom"
        ))
        .send()
        .context("не удалось загрузить atom feed с GitHub")?;

    if resp.status().as_u16() != 200 {
        return Ok(None);
    }

    let body = resp
        .text()
        .context("не удалось прочитать body atom feed")?;

    // Парсим XML вручную — достаём ссылки из тегов <link>
    // Atom feed содержит ссылки вида:
    // <link rel="alternate" href="https://github.com/myakoto/neptune/releases/tag/v0.2.3" />
    let tag_name = extract_atom_tag(&body)?;
    if tag_name.is_empty() {
        return Ok(None);
    }
    // Извлекаем номер версии из URL tag/xxx
    let version = tag_name
        .strip_prefix(&format!(
            "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/"
        ))
        .unwrap_or(&tag_name)
        .to_owned();

    let exe_url = format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{version}/{ASSET_EXE}"
    );
    let sha256_url = format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{version}/{ASSET_SHA256}"
    );

    Ok(Some(ReleaseInfo {
        version,
        exe_url,
        sha256_url,
    }))
}

/// Извлекает href из тегов <link> в atom feed.
fn extract_atom_tag(xml: &str) -> Result<String> {
    let link_start = xml.find("<link").ok_or_else(|| anyhow::anyhow!("нет тегов link в atom"))?;
    let link_end = xml[link_start..].find(" />").unwrap_or_else(|| xml[link_start..].find("\"/>")?);
    let link_content = &xml[link_start..][..=link_end];

    // Ищем href="..."
    let href_start = link_content.find("href=\"").ok_or_else(|| anyhow::anyhow!("нет href в link"))?;
    let href_body = &link_content[href_start + 6..];
    let href_end = href_body.find("\"").ok_or_else(|| anyhow::anyhow!("нет закрывающей кавычки href"))?;
    Ok(href_body[..href_end].to_owned())
}

/// Свежий стабильный релиз; `None`, если релизов ещё нет.
///
/// # Errors
/// Возвращает ошибку при недоступности GitHub или битом ответе.
pub fn latest_release() -> Result<Option<ReleaseInfo>> {
    // 1) Сначала пробуем кэш: не тратим запросы если свежий
    if let Some(cache) = load_cache() {
        return Ok(Some(ReleaseInfo {
            version: cache.version,
            exe_url: cache.exe_url,
            sha256_url: cache.sha256_url,
        }));
    }

    // 2) Основной путь: REST API
    if let Ok(Some(release)) = latest_release_api() {
        save_cache(&release);
        return Ok(Some(release));
    }

    // 3) Fallback: atom feed
    if let Ok(Some(release)) = latest_release_rss() {
        save_cache(&release);
        return Ok(Some(release));
    }

    // 4) Если ничего не получилось — вернуть кэш несмотря на просрочку
    if let Some(cache) = load_cache() {
        log_update("fallback: использован просроченный кэш");
        return Ok(Some(ReleaseInfo {
            version: cache.version,
            exe_url: cache.exe_url,
            sha256_url: cache.sha256_url,
        }));
    }

    log_update("fallback: кэш отсутствует, нет релиза");
    Ok(None)
}

/// Запрос к GitHub REST API с проверкой статуса и текстом ошибки.
fn latest_release_api() -> Result<Option<ReleaseInfo>> {
    let response = client()?.get(api_latest()).send().context(
        "не удалось спросить GitHub о последнем релизе",
    )?;

    let status = response.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        let body_preview = response
            .text()
            .context("не удалось прочитать ответ GitHub")?;
        anyhow::bail!(
            "GitHub API вернул статус {status}: {body_preview}",
        );
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
    Ok(version_is_newer(env!("CARGO_PKG_VERSION"), &release.version).then_some(release.version))
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
        anyhow::bail!("контрольная сумма не совпала: ждали {expected_hash}, скачали {actual_hash}");
    }

    self_replace::self_replace(&exe_path).context("не удалось подменить бинарник")?;
    Ok(release.version)
}

/// Путь к логу обновлений (рядом с запущенным exe).
pub fn log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join("update.log")))
        .unwrap_or_else(|| std::env::temp_dir().join("neptune-update.log"))
}

/// Пишет строку с временной меткой в лог обновлений рядом с exe.
pub fn log_update(msg: &str) {
    use std::io::Write;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = writeln!(file, "[{secs}] {msg}");
    }
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
    let mut file =
        File::create(dest).with_context(|| format!("не удалось создать {}", dest.display()))?;
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
        .find(|token| token.len() == SHA256_HEX_LEN && token.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

/// Считает sha256 файла в hex-строке.
///
/// # Errors
/// Возвращает ошибку чтения файла.
fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("не удалось открыть {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut hex = String::with_capacity(SHA256_HEX_LEN);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
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
    match (current_pre.as_deref(), candidate_pre.as_deref()) {
        (Some(current_pre), Some(candidate_pre)) => candidate_pre > current_pre,
        // Стабильный релиз новее своего пререлиза.
        (Some(_), None) => true,
        // Пререлиз стабильной версии — старее её.
        (None, _) => false,
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
        let text =
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  neptune.exe\n";
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
        std::fs::write(&path, b"abc").expect("запись файла");
        assert_eq!(
            sha256_file(&path).expect("хеш файла"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn core_comparison_pads_missing_parts() {
        assert_eq!(compare_core(&[0, 2], &[0, 2, 0]), std::cmp::Ordering::Equal);
        assert_eq!(
            compare_core(&[0, 3], &[0, 2, 9]),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn repo_constants_match_release_assets() {
        assert!(RELEASE_JSON.contains(ASSET_EXE));
        assert!(RELEASE_JSON.contains(ASSET_SHA256));
        assert_eq!(REPO_OWNER, "myakoto");
        assert_eq!(REPO_NAME, "neptune");
    }
}
