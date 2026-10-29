<div align="center">

# Singularity

**Клиент-серверный игровой движок для многопользовательских песочниц на Rust поверх Bevy.**

[![CI](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml/badge.svg)](https://github.com/mel0k1/Singularity/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-b7410e.svg)](https://www.rust-lang.org)

Rust-переосмысление [Robust Toolbox](https://github.com/space-wizards/RobustToolbox) — движка Space Station 14.

[English](README.md) · [Русский](README.ru.md)

</div>

## Зачем

Robust Toolbox десять лет доказывал необычный тезис: полностью авторитарный сервер, репликация мира дельта-снапшотами с индивидуальной видимостью на каждого игрока, клиентское предсказание и контент на YAML способны нести большую многопользовательскую песочницу. Singularity сохраняет эти идеи и пересобирает их на Rust.

- **Авторитарный сервер** — клиент не решает ничего критичного для геймплея сам
- **Снапшот-репликация** — мир передаётся дельта-снапшотами с potentially visible set на каждого игрока, а не RPC-супом
- **Предсказание и согласование** — ввод ощущается мгновенным даже при большом пинге
- **Контент на данных** — сущности, прототипы и карты — обычный YAML
- **Bevy внутри** — ECS, планировщик, рендер, аудио и ввод берутся из Bevy там, где это уместно

## Крейты

| Крейт | Роль | Заменяет (Robust Toolbox) |
| --- | --- | --- |
| `singularity-core` | общий словарь: тики, прототипы, сериализация, шина событий | `Robust.Shared` |
| `singularity-server` | авторитарная симуляция, PVS-снапшоты, сессии игроков | `Robust.Server` |
| `singularity-client` | рендер, ввод, UI, аудио, предсказание | `Robust.Client` |

## Статус

Фаза исследования. Исходники Robust Toolbox, порядка 367 тыс. строк C#, изучены подсистема за подсистемой и отображены на примитивы Rust и Bevy. Играбельного кода пока нет.

## Дорожная карта

1. Ядро общего словаря на Bevy ECS: тики, сетевые id сущностей, прототипы, YAML-сериализация
2. Сервер: фиксированный тик-цикл, протокол снапшотов, видимость на игрока
3. Клиент: предсказание, согласование, интерполяция
4. Рендер и ввод поверх `bevy_render`
5. UI, аудио, карты, инструментарий

## Сборка

```sh
cargo build --release
cargo test --all
```

## Стиль кода

Код в репозитории намеренно без комментариев: весь смысл несут имена и структура.

## Лицензия

Apache-2.0 — см. [LICENSE](LICENSE). Singularity — независимый проект, вдохновлённый [Robust Toolbox](https://github.com/space-wizards/RobustToolbox) от [space-wizards](https://github.com/space-wizards) и [Bevy](https://bevyengine.org).
