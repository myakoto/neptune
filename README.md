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
