# Neptune

Живой переводчик речи для созвонов под Windows (Rust).

Направления работы:
- **Мне говорят** — звук созвона (WASAPI loopback) → Deepgram → Yandex Translate → русские субтитры;
- **Я говорю** — push-to-talk с микрофона → распознанный русский → английский текст в буфере обмена.

Сейчас реализован первый вертикальный срез в виде CLI: перевод текста и
распознавание готового WAV-файла. Дальше: стриминговый клиент Deepgram,
захват звука, GUI-окно.

## Настройка

```
cp .env.example .env   # и заполнить ключи
```

| Переменная | Откуда |
|---|---|
| `DEEPGRAM_API_KEY` | console.deepgram.com |
| `YANDEX_API_KEY` | API-ключ сервисного аккаунта Yandex Cloud с ролью `ai.translate.user` |

## Использование

```
cargo run -- translate "привет, как дела"        # ru -> en
cargo run -- translate --from en --to ru "hello"
cargo run -- transcribe path\to\audio.wav        # автоопределение языка
cargo run -- transcribe --language ru audio.wav
```

## Проверки качества

Всё обязано проходить перед коммитом:

```
cargo fmt --check                          # форматирование
cargo clippy --all-targets -- -D warnings  # линтер (pedantic включён в Cargo.toml)
cargo test                                 # тесты
```

Автоисправление форматирования: `cargo fmt`.

## Версии и релизы

Версия — semver, источник правды — `version` в Cargo.toml. Релизы собирает
GitHub Actions ([release.yml](.github/workflows/release.yml)) по тегу `v*`;
CI ([ci.yml](.github/workflows/ci.yml)) гоняет fmt/clippy/test на каждый пуш.

Выпустить релиз:

1. Поднять `version` в Cargo.toml (например, `0.2.0`), закоммитить.
2. `git tag v0.2.0 && git push origin master v0.2.0`
3. Actions соберёт exe, упакует в zip и опубликует GitHub Release.
   Workflow упадёт, если тег не совпадает с версией в Cargo.toml.

Бета: версия и тег с суффиксом (`0.3.0-beta.1` / `v0.3.0-beta.1`) — релиз
помечается prerelease. Автообновление такие версии **не** предлагает:
приложение обновляется только до последнего стабильного релиза.

Автообновление: при старте GUI проверяет GitHub Releases; если есть версия
новее — в окне появляется баннер «обновить», после установки — кнопка
перезапуска. Вручную: `neptune update`.

## Структура

```
src/
  main.rs           точка входа (минимальная)
  cli.rs            разбор аргументов, диспетчер команд
  config.rs         ключи API из .env / окружения
  translate/        перевод текста (Yandex Cloud Translate v2)
  stt/              распознавание речи (Deepgram Nova-3)
```

Правило: каждый модуль — одна зона ответственности, свои типы ошибок
(`thiserror`) и юнит-тесты рядом с кодом. Сетевые вызовы отделены от чистой
логики (сборка URL, разбор ответов) — тесты гоняются без сети.
