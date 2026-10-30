<div align="center">

# Singularity

**Гибридный клиент-серверный игровой движок: C#-ядро оркестрации и Rust-подсистемы производительности.**

[![CI](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml/badge.svg)](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-b7410e.svg)](https://www.rust-lang.org)

Переосмысление [Robust Toolbox](https://github.com/space-wizards/RobustToolbox) — движка Space Station 14.

[English](README.md) · [Русский](README.ru.md)

</div>

## Почему гибрид

Переписать Robust Toolbox на чистом Rust — значит заменить ~367 тыс. строк C#: один `Robust.Shared` — это 1073 файла и ~165 тыс. строк. Контракт контента приварен к C#: дискавери через рефлексию плюс четыре Roslyn-генератора (`[DataField]`, `[AutoGenerateComponentState]`, `[SubscribeLocalEvent]`, `[Dependency]`) генерируют код прямо в сборки контента. Полный перепис ломает каждый контент-проект, когда-либо написанный под движок.

Singularity режет движок по шву, которым Robust Toolbox уже пользуется в продакшене:

- **C# остаётся оркестратором** — ECS, IoC, YAML-сериализация, прототипы, оркестрация GameStates/PVS, сетевые сообщения. Здесь живёт контракт контента, а значит — совместимость.
- **Rust становится поставщиком нативного слоя** — Robust Toolbox уже возит нативный код за C#-интерфейсами (zstd, libsodium, SDL3, OpenAL). Singularity обобщает паттерн: ресурсоёмкие подсистемы — это Rust-крейты за тонкими C#-интерфейсами.
- **Один бинарник на обоих концах** — клиент и сервер используют один и тот же Rust-артефакт: физика и криптография ведут себя одинаково на обеих сторонах. Детерминизм прежде всего.

## Архитектура

```text
C#-слой — оркестрация и контракт контента            Rust-слой — крейты за C#-интерфейсами
══════════════════════════════════════════            ══════════════════════════════════════════
GameObjects/ECS · IoC · шина событий                 singularity-crypto         кадры XChaCha20-Poly1305
YAML-сериализация · прототипы                       singularity-physics        изучение Rapier/Avian
GameStates · оркестрация PVS                        singularity-net-transport  спайк шва сделан
сетевые сообщения · StringTable · дискавери         singularity-pvs            SoA-данные сессий
                        └──────────── FFI-контракт ─────────┘
```

FFI-контракт: C# собирает реестры — типы компонентов, сетевые id, хэши сериализатора — и передаёт их в Rust таблицами. Rust никогда не рефлексирует; реестры всегда приходят от C#-хоста.

## Совместимость на проводе

`singularity-crypto` повторяет `NetEncryption` из Robust Toolbox байт в байт:

- кадр = `[u64 LE счётчик][XChaCha20-Poly1305 шифротекст + тег]`, накладные расходы 24 байта
- 24-байтовый XChaCha-нонс — счётчик в первых 8 байтах, остальное нули
- счётчики пре-инкрементируются на два: сервер передаёт 2, 4, 6…, клиент 3, 5, 7… — направления никогда не пересекаются
- ошибка аутентификации — это значение, а не исключение: `decrypt` возвращает `None`, как `TryDecrypt`; битый кадр не может уронить узел

## Структура

| Путь | Язык | Роль |
| --- | --- | --- |
| `rust/crates/singularity-crypto` | Rust | кодек кадров XChaCha20-Poly1305, совместим с `NetEncryption` на уровне байтов |
| `rust/crates/singularity-physics` | Rust | исследование бэкенда физики: Rapier и Avian за общим швом `PhysicsBackend`, с бенчмарком и матрицей паритета — см. [docs/physics-bench.md](docs/physics-bench.md) |
| `rust/crates/singularity-physics-ffi` | Rust | C ABI-шов: `SgWorld` на карту, командный буфер, массовая выгрузка состояний и событий — нативная сторона `IPhysicsBackend` |
| `rust/crates/singularity-net-transport` | Rust | шов под `INetChannel`: UDP-транспорт, рукопожатие, stop-and-wait надёжность, событийный опрос, C ABI — см. [docs/net-transport.md](docs/net-transport.md) |
| `rust/crates/singularity-pvs` · план | Rust | SoA-данные видимости на сессию |
| `csharp/engine` | C# | вендор RobustToolbox @ fdb576c — стартовая точка ядра оркестрации; несёт шов `IPhysicsBackend` за `SharedPhysicsSystem.SimulateWorld()` |
| `csharp/benches/Singularity.PhysicsBench` | C# | Farseer и rapier-зеркало замерены в лоб на реальном ECS — см. [docs/physics-bench.md](docs/physics-bench.md) |

## Статус

Фаза исследования завершена. Исходники Robust Toolbox изучены подсистема за подсистемой — ECS, IoC, сериализация, прототипы, GameStates/PVS, сеть, физика, нативные границы. Три спайка реализованы, протестированы и замерены: `singularity-crypto`, `singularity-physics` и зеркало внутри движка — `IPhysicsBackend` с Rapier-бэкендом за `SimulateWorld()`, переключается cvar-ом `physics.backend`, по умолчанию выключен. Второй раунд закрыл весь список вердикта: джоинты distance/revolute/prismatic/weld с разрывами и `CollideConnected`, `PreventCollideEvent` как кэшированный обратный FFI-вызов через родной `ShouldCollideSlow`, инкрементальный сбор тел (бит-в-бит идентичен полному скану во всех сценах, в 17 раз меньше оркестрации на падающих сценах) и движковые cvar-ы сна, отображённые на пороги Rapier. Те же сцены, тот же ECS: 2,3× по p50 на контактно-тяжёлой сцене floor, провал KinematicController исправлен полностью, в покое спит в 2,5 раза больше тел, чем на Farseer, детерминизм двойного прогона бит-в-бит — см. [docs/physics-bench.md](docs/physics-bench.md).

## Дорожная карта

1. `singularity-crypto` — сделано
2. `singularity-physics` — сделано и внедрено: `IPhysicsBackend` за `SimulateWorld()`, rapier-зеркало замерено в лоб ([docs/physics-bench.md](docs/physics-bench.md)) — джоинты, парный фильтр, инкрементальный сбор и тюнинг сна сделаны; остался паритет предсказаний через общий бинарь
3. `singularity-net-transport` — спайк шва сделан ([docs/net-transport.md](docs/net-transport.md)): UDP-транспорт, событийная модель, FFI и stop-and-wait надёжность под измеренными потерями; порт фреймера Lidgren — следующий этап
4. `singularity-pvs` — SoA-данные сессий; pinned `PvsData` уже неуправляемые
5. Подключение C#-ядра — сделано: движок завендорен, Farseer замерен в лоб, детерминизм подтверждён

## Сборка

```sh
cd rust
cargo build --release
cargo test --all
cargo run --release -p singularity-physics --bin bench
cargo run --release -p singularity-net-transport --bin bench
```

```sh
cd csharp
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --validate
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --validate --strict --backend=rapier
dotnet run --project benches/Singularity.PhysicsBench -c Release -- --scene=all --verify-sync
```

## Стиль кода

Код в репозитории намеренно без комментариев: весь смысл несут имена и структура. CI отклоняет комментарии в исходниках. Коммиты короткие.

## Лицензия

Apache-2.0 — см. [LICENSE](LICENSE). Singularity — независимый проект, вдохновлённый [Robust Toolbox](https://github.com/space-wizards/RobustToolbox) (MIT) от [space-wizards](https://github.com/space-wizards) и [Bevy](https://bevyengine.org) (MIT или Apache-2.0).
