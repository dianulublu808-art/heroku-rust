# Функциональный паритет Heroku (Python) и heroku-rust

Документ составлен по исходному коду обоих проектов, а не по README/ROADMAP.
Проверка Rust-части: `cargo fmt --all -- --check`, `cargo check --all-targets`,
`cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test --all-targets` (88 тестов на момент составления).

Статусы:

- **COMPLETE** — поведение реализовано и покрыто тестами/проверкой кода.
- **PARTIAL** — основная возможность есть, но часть поведения оригинала отсутствует.
- **MISSING** — возможности нет.
- **DIFFERENT** — работает иначе по архитектурным причинам.
- **UNVERIFIED** — код есть, но реальное поведение (обычно Telegram API) не проверено.

> Важно: `COMPLETE` не означает побайтовое совпадение с Python. Для команд с
> внешними сервисами (перевод, eval, terminal) проверялась архитектура и
> обработка ошибок, а не конкретный ответ сервиса.

## 1. Ядро и команды

| Возможность | Оригинал | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- | --- |
| Диспетчер команд и aliases | `heroku/dispatcher.py` | `src/dispatcher.rs`, `src/module.rs` | COMPLETE | — |
| Покрытие имён команд | ~87 команд | 96 зарегистрированных имён/aliases | COMPLETE | Сверять поведение сложных команд, а не только имена |
| Prefix/`.prefix`/`.resetprefix` | есть | есть | COMPLETE | — |
| Reply/edit/HTML/grep-пайпы | есть | `src/tg.rs` | COMPLETE | — |
| Reply-кнопки и callback-кнопки | есть | `Button`, `ReplyMarkup` | COMPLETE | — |
| Ограничение конкурентности апдейтов | asyncio-задачи | 64 задачи + backpressure | COMPLETE | — |
| Graceful shutdown | есть | дренаж 10 с, abort, `on_unload` | COMPLETE | — |
| Собственные команды Rust | — | +15 | DIFFERENT | Документировать в help (сделано) |
| Rate limiting / API protection | `api_protection.py` | `src/protection.rs`, `api_ratelimiter` | COMPLETE | — |
| Flood/anti-spam | есть | `protection.rs` | COMPLETE | — |
| `.terminal` | есть | таймаут 600 с, лимит 3500 символов | COMPLETE | Фильтр строк не является песочницей |
| `.eval` | Python eval | Rhai | DIFFERENT | Часть Python-сниппетов несовместима |
| `.translate` | внешний API | внешний API | COMPLETE | Нет live-теста с сетью |
| `.update`/`.rollback` | git | git | COMPLETE | — |

## 2. Встроенные модули

| Возможность | Оригинал | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- | --- |
| 19 встроенных модулей | `heroku/modules/*.py` | `src/modules/*.rs` | PARTIAL | Часть модулей разбита/объединена; поведение отдельных команд требует сверки |
| `.help` и карточки модулей | есть | есть, с security-маской | COMPLETE | — |
| `.settings`, `.togglemod`, `.togglewatcher` | есть | есть, lifecycle без рестарта | COMPLETE | — |
| `.logs` по модулям | есть | есть, включая внешние `heroku::ext_*` | COMPLETE | — |
| `.backupall` / `.restoreall` | ZIP | ZIP, Python-совместимый | COMPLETE | — |
| `.quickstart` | есть | есть | PARTIAL | Нет live-проверки первого запуска |
| `loader_restrictor` (квиз) | есть | есть, poll vote через raw update | PARTIAL | Нет live-проверки Telegram-квиза |
| `.inline` bot management | `inline_stuff.py` | `src/modules/inline.rs` | PARTIAL | См. раздел Inline |
| Python-модули | загружаются | не исполняются | MISSING | Только порт на Rust; это осознанное ограничение |

## 3. Module API

| Возможность | Оригинал | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- | --- |
| Команды, aliases, args | есть | `CommandDef`, `Args` | COMPLETE | — |
| Config spec и defaults | есть | `ConfigSpec`/`ConfigKind` | COMPLETE | — |
| Зависимости модулей | есть | `dependencies()`, топологическая загрузка | COMPLETE | — |
| Lifecycle `on_load`/`on_unload` | есть | транзакционный `lifecycle::load` | COMPLETE | — |
| `on_error` | есть | есть для команд, watcher, callback, inline | COMPLETE | — |
| Failed-состояние модуля | частично | failed-модули не получают dispatch/help/inline/callbacks | COMPLETE | — |
| Managed-задачи модуля | нет прямого аналога | `Ctx::spawn_module_task`, лимит 256, abort при unload | COMPLETE | — |
| Версионирование API | есть | `MODULE_API_VERSION = 3` | COMPLETE | Миграция 2 → 3: добавлено поле `WatcherFilter::func` |
| Watchers | есть | `watchers()` + `on_watcher()`, включая `func` | COMPLETE | — |
| Callback handlers | есть | `has_callback`/`on_callback` | COMPLETE | — |
| Inline handlers | есть декоратор | `inline_handlers()` + `on_inline_query()` | PARTIAL | Нет live-проверки |
| Raw TL handlers | есть | нет | MISSING | Требует отдельного API и транспортной абстракции |
| Горячая загрузка внешних модулей | Python import | пересборка `.rs` | DIFFERENT | Динамическая загрузка Rust-кода небезопасна/невозможна без ABI |
| Документация для авторов | частичная | `modules/README.md`, `docs/PORTING.md` | PARTIAL | Дополнить примерами Inline 2.0 и ошибок |

## 4. Watcher API

| Фильтр | Оригинал | Rust | Статус |
| --- | --- | --- | --- |
| `no_pm` / `only_pm` | есть | есть | COMPLETE |
| `no_channels` / `only_channels` | есть | есть | COMPLETE |
| `no_groups` / `only_groups` | есть | есть | COMPLETE |
| `only_inline` / `no_inline` | есть (`via_bot_id`) | есть (`Msg::via_bot_id`) | COMPLETE |
| `no_stickers` / `only_stickers` | есть | есть | COMPLETE |
| `no_docs` / `only_docs` | есть | есть | COMPLETE |
| `no_audios` / `only_audios` | есть | есть | COMPLETE |
| `no_videos` / `only_videos` | есть | есть | COMPLETE |
| `no_photos` / `only_photos` | есть | есть | COMPLETE |
| `no_forwards` / `only_forwards` | есть | есть | COMPLETE |
| `no_reply` / `only_reply` | есть | есть | COMPLETE |
| `mention` / `no_mention` | есть | есть | COMPLETE |
| `startswith` / `endswith` / `contains` | есть | есть | COMPLETE |
| `regex` | есть | есть, regex валидируется при загрузке | COMPLETE |
| `from_id` / `chat_id` | есть | есть | COMPLETE |
| `outgoing`, `editable` | есть | есть | COMPLETE |
| `func` | произвольный callable | `WatcherFilter::func` / `with_func` | COMPLETE |
| Комбинации фильтров | AND | AND, `func` вызывается последним | COMPLETE |
| Редактирования | есть | `editable` | COMPLETE |
| Изоляция ошибок watcher-ов | есть | `on_error`, dispatch продолжается | COMPLETE |

## 5. Inline Manager 2.0

| Возможность | Оригинал | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- | --- |
| Публичный API форм | `InlineManager.form` | `ctx.inline.form(...).send(...)` | PARTIAL | Rust отправляет форму как сообщение бота в чат; Python использует inline-режим, поэтому бот должен быть в чате/диалоге |
| Публичный API списков | `InlineManager.list` | `ctx.inline.list(items)` | COMPLETE | — |
| Публичный API галерей | `InlineManager.gallery` | `ctx.inline.gallery()` | PARTIAL | Слайды с медиа отправляются сообщением бота; нативные inline-photo результаты не реализованы |
| QueryGallery | `InlineManager.query_gallery` | `ctx.inline.query_gallery()` | PARTIAL | Статьи с thumb + unit; после выбора `InlineSend` привязывает inline message id |
| Пагинация | `build_pagination` | `navigation()` + `InlineButton::Page` | COMPLETE | — |
| Кнопки callback/url/switch/close | есть | есть | COMPLETE | — |
| Callback handlers модулей | `_custom_map` | `on_callback` на unit + `Module::on_callback` | COMPLETE | — |
| Inline handlers модулей | декоратор | `Module::inline_handlers`/`on_inline_query` | PARTIAL | Нет live-теста |
| Фото | URL | `InlineMedia::Photo` (bot message) | PARTIAL | Нет inline-photo результата и загрузки файлов |
| GIF | URL | `InlineMedia::Gif` | PARTIAL | Отправляется как external document, не как animation |
| Видео | URL | `InlineMedia::Video` | PARTIAL | External document без video-атрибутов/стриминга |
| Аудио | URL | `InlineMedia::Audio` | PARTIAL | External document без audio-атрибутов |
| Файл | URL + mime | `InlineMedia::File` | PARTIAL | MIME не передаётся для external URL |
| Location | lat/long | `InlineMedia::Location` | UNVERIFIED | Код есть, live-проверки нет |
| TTL юнитов | 24 ч/явный TTL | `DEFAULT_UNIT_TTL`, `ttl()` | COMPLETE | — |
| Автоочистка | cleaner каждые 5 с | tokio cleaner каждые 5 с | COMPLETE | — |
| Повторные callback | dedup | `(sender, query_id)`, 30 с, 512 | COMPLETE | — |
| Владелец юнита | caller/`force_me` | owner/sudo + `force_me` | COMPLETE | — |
| Permissions перед callback | security map | `InlineUnit::allows` + module `callback_allowed` | COMPLETE | — |
| Конкурентные callback | asyncio | mutex registry, handler вне lock, тест конкурентных page update | PARTIAL | Нет теста с одновременными реальными Telegram callback |
| Некорректные callback data | проверки | парсер `hu:` отклоняет мусор | COMPLETE | — |
| Ошибки Telegram API | логирование | `Result`, лог, alert пользователю | PARTIAL | Нет fault-injection теста |

### 5.1. Что именно добавлено в этой итерации

- `src/inline_units.rs`: реестр юнитов, TTL, cleaner, безопасность, парсер
  callback-токенов, формы/списки/галереи/query-gallery.
- `InlineManager::form/list/gallery/query_gallery` — публичные конструкторы.
- `InlineMessageRef::{edit,delete,unload}`.
- `InlineButton`: callback, URL, switch-inline, page, close.
- `InlineMedia`: photo/gif/video/audio/file/location.
- `InlineCallbackAction`: Handled/Toast/Alert/Edit/GoTo/Close/Unload.
- `Module::inline_handlers` и `Module::on_inline_query` с
  `InlineQueryReply::{Skip,Results,Answered}`.
- `Update::InlineSend` привязывает выбранный inline-результат к unit.
- TTL ограничен 24 часами; реестр ограничен 256 юнитами.
- Callback-права: owner/sudo/always_allow/disable_security; `force_me`.
- Тесты: парсер, TTL, очистка, bound, конкурентные page-update, рендер
  callback-токенов, валидация медиа, list/gallery пагинация, unload hook.

## 6. Config, database, accounts

| Возможность | Оригинал | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- | --- |
| JSON-конфиг owner→key→value | есть | `data/heroku.json` | COMPLETE | — |
| Config API модулей | есть | `config_spec` + inline-редактор | COMPLETE | — |
| Атомарная запись | есть | tmp + fsync + rename + mutex | COMPLETE | — |
| Сохранение повреждённой БД | есть | `*.corrupt-<ts>` | COMPLETE | — |
| Версия схемы | нет/частично | `heroku.meta.schema = 1`, read-only для новой версии | COMPLETE | — |
| Миграции `hikka.*`/`legacy.*` | есть | есть + снимок перед миграцией | COMPLETE | — |
| Восстановление после обрыва записи | нет | truncated/stale tmp тесты | COMPLETE | — |
| Multi-account registry | есть | `accounts.json`, process-wide lock, atomic update | COMPLETE | — |
| Изоляция аккаунтов | есть | отдельные `accounts/<id>/heroku.json` и сессия | COMPLETE | — |
| Переключение без рестарта | есть | только рестарт процесса | MISSING | Нужен supervisor на аккаунт и безопасная передача runtime |
| Восстановление после отключения Telegram | есть | reconnect loop inline-бота; основной клиент — grammers | PARTIAL | Нет integration-теста network recovery |

## 7. Security, backup, errors

| Возможность | Оригинал | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- | --- |
| Bitmask permissions | есть | совместимы | COMPLETE | — |
| Owner/sudo/security groups | есть | есть | COMPLETE | — |
| Callback permission checks | есть | unit + module + built-in cards | COMPLETE | — |
| `.help` по маске | есть | есть | COMPLETE | — |
| Секреты в логах | redaction | токены, пароли, `api_hash`, Bearer, URL credentials | COMPLETE | — |
| Бэкапы Python-совместимые | ZIP | ZIP | COMPLETE | — |
| Восстановление бэкапа | есть | есть | COMPLETE | — |
| Изоляция ошибок модулей | есть | `on_error` + failed-state | COMPLETE | — |
| Ошибки Telegram API | логирование | `anyhow::Context`, warn/error | PARTIAL | Нет системного fault-injection набора |

## 8. Тесты, CI, производительность

| Возможность | Rust | Статус | Недостающая работа |
| --- | --- | --- | --- |
| Unit-тесты | 88 тестов | COMPLETE | — |
| Тесты БД/миграций/повреждений | есть | COMPLETE | — |
| Тесты multi-account | есть | COMPLETE | — |
| Тесты watcher-фильтров | есть, включая короткое замыкание `func` | PARTIAL | Нет тестов на реальных `Msg`/inline-via-bot (требуется mock-транспорт) |
| Тесты callback/TTL/ownership | есть | COMPLETE | — |
| Mock Telegram API | нет | MISSING | Нужна транспортная абстракция: grammers 0.12 не даёт публичных конструкторов `Client`/`PeerMap`/`Message` |
| Интеграционные тесты диспетчера | нет | MISSING | После mock-транспорта |
| CI fmt/check/clippy/test/release | `.github/workflows/ci.yml` | UNVERIFIED | Прогон на GitHub Actions не выполнялся в этой среде |
| Regex cache | есть, bound 256 | COMPLETE | — |
| Ограничение задач | 64 update + 256/module | COMPLETE | — |
| Профилирование hot path | нет | MISSING | Сначала измерения, потом оптимизации |
| Утечки кешей | bound для regex/callback/units | PARTIAL | Нет long-running soak-теста |

## 9. Осознанные архитектурные отличия

1. Python-модули не исполняются. Модуль — это Rust-крейт/файл, компилируемый в
   бинарник. Динамическая загрузка Python не будет реализована без отдельного
   интерпретатора.
2. Inline-формы Rust отправляются сообщением бота, а не через inline-режим.
   Это ограничение текущего `InlineManager`: бот должен иметь диалог/доступ к
   чату. Python-форма может отправляться в любой чат через inline query.
3. Медиа в inline-результатах остаётся article-based (title/description/thumb).
   Нативные `InputBotInlineResultPhoto`/`Document` требуют загрузки файла или
   `InputPhoto`/`InputDocument`, которых у нас пока нет.
4. `quote_media` — ссылка с preview, а не нативная цитата (ограничение grammers).
5. `.eval` — Rhai, а не Python.
6. `WatcherFilter::func` принимает Rust-closure `Fn(&Msg) -> bool`, а не
   произвольный Python-callable. Предикат выполняется после декларативных
   фильтров, поэтому дорогие проверки не вызываются для заведомо неподходящих
   сообщений.

## 10. Следующие приоритеты

1. **Inline 2.0, оставшееся:** загрузка медиа для нативных inline-результатов,
   live-проверка location/gallery, fault-injection тесты Telegram API,
   конкурентные callback-тесты на уровне диспетчера.
2. **Watcher API:** тесты `func`, `only_inline`/`no_inline`, редактирований и
   комбинаций на реальных `Msg` (после mock-транспорта).
3. **Mock Telegram:** транспортная абстракция вокруг `Client`/`Msg`, затем
   integration-тесты диспетчера, callback-ов, reconnect.
4. **Multi-account:** supervisor на аккаунт, безопасное live-switching.
5. **Performance:** профилирование dispatcher hot path, аллокаций, mutex
   contention; только после измерений.
