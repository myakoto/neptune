//! Автообновление через GitHub Releases (myakoto/neptune).
//!
//! Релизы публикует workflow release.yml; артефакт — zip с neptune.exe.
//! Prerelease-версии (теги с дефисом) API «latest release» не отдаёт,
//! поэтому автообновление видит только стабильные релизы.
//!
//! Обе функции блокирующие (`self_update` использует blocking-reqwest):
//! из tokio-кода звать только через `spawn_blocking`.

use anyhow::{Context, Result};
use self_update::backends::github::{Update, UpdateBuilder};
use self_update::cargo_crate_version;

const REPO_OWNER: &str = "myakoto";
const REPO_NAME: &str = "neptune";
/// Подстрока имени артефакта из release.yml.
const ASSET_TARGET: &str = "x86_64-pc-windows-msvc";

fn configure() -> UpdateBuilder {
    let mut builder = Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("neptune.exe")
        .target(ASSET_TARGET)
        .current_version(cargo_crate_version!())
        .no_confirm(true)
        .show_output(false)
        .show_download_progress(false);
    builder
}

/// Возвращает версию свежего стабильного релиза, если он новее текущей.
///
/// # Errors
/// Возвращает ошибку при недоступности GitHub API или битом ответе.
pub fn check() -> Result<Option<String>> {
    let updater = configure()
        .build()
        .context("не удалось настроить проверку обновлений")?;
    let latest = match updater.get_latest_release() {
        Ok(release) => release,
        // 404 — в репозитории ещё нет ни одного релиза: обновлений нет.
        Err(error) if error.to_string().contains("404") => return Ok(None),
        Err(error) => {
            return Err(anyhow::Error::new(error).context("не удалось узнать последний релиз"));
        }
    };
    let newer = self_update::version::bump_is_greater(cargo_crate_version!(), &latest.version)
        .context("не удалось сравнить версии")?;
    Ok(newer.then_some(latest.version))
}

/// Скачивает свежий релиз и подменяет текущий exe. Возвращает новую версию.
///
/// # Errors
/// Возвращает ошибку сети, распаковки или подмены бинарника.
pub fn apply() -> Result<String> {
    let status = configure()
        .build()
        .context("не удалось настроить обновление")?
        .update()
        .context("обновление не удалось")?;
    Ok(status.version().to_owned())
}
