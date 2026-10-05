# temandroid/rustdesk-server

Self-hosted сервер RustDesk (**hbbs** / **hbbr**) для связки с [rustdesk-api-srv](https://github.com/temandroid/rustdesk-api-srv).

База — открытый [rustdesk/rustdesk-server](https://github.com/rustdesk/rustdesk-server) (AGPL). В этой сборке дополнительно:

1. **HTTP API proxy** — клиент может ходить в ваш API через TCP rendezvous (`HttpProxyRequest`), когда прямой доступ к API закрыт (firewall, CGNAT).
2. **secure_tcp / KeyExchange** — залогиненные клиенты ≥1.4.1 нормально устанавливают защищённый TCP к hbbs (без этого бывает `Failed to secure tcp: deadline has elapsed`).

Образы: `ghcr.io/temandroid/rustdesk-server:latest` (и тег с коротким SHA коммита). При настроенных секретах CI также пушится на Docker Hub `temandroid/rustdesk-server`.

Полный список переменных upstream: [docs/environment-variables.md](docs/environment-variables.md).

---

## Быстрый старт (Docker)

На сервере с Docker:

```bash
mkdir -p /data/rustdesk/data
cd /data/rustdesk
curl -fsSL -o docker-compose.yml \
  https://raw.githubusercontent.com/temandroid/rustdesk-server/master/docker-compose.yml

# при необходимости создайте .env рядом:
#   API_SERVER=http://127.0.0.1:21114
#   RUST_LOG=info

docker compose pull
docker compose up -d
```

Compose использует `network_mode: host` (порты 21115–21119 на хосте). Данные и ключи — в `./data` (внутри контейнера `/root`).

Публичный ключ для клиентов и api-srv:

```bash
cat data/id_ed25519.pub
```

Откройте в firewall UDP/TCP **21116**, TCP **21117**, при WebSocket — **21118/21119**, для NAT test — **21115**.

### Связка с rustdesk-api-srv

| Где | Что указать |
|---|---|
| api-srv `.env` | `ID_SERVER=<этот хост>`, `RUSTDESK_KEY=<id_ed25519.pub>` |
| hbbs | `API_SERVER=http://127.0.0.1:21114` если API на той же машине; иначе `https://ваш-api-домен` |
| клиент | ID/Relay = этот хост, API = `PUBLIC_URL` api-srv, Key = тот же pubkey |

---

## Установка без Docker (бинари из исходников)

Нужны Rust (stable), `pkg-config`, OpenSSL, CMake, C++ toolchain.

```bash
git clone --recursive https://github.com/temandroid/rustdesk-server.git
cd rustdesk-server
cargo build --release
```

Бинарники в `target/release/`:

| Файл | Назначение |
|---|---|
| `hbbs` | ID / rendezvous |
| `hbbr` | relay |
| `rustdesk-utils` | утилиты |

Запуск (пример, рабочий каталог с ключами):

```bash
mkdir -p /var/lib/rustdesk && cd /var/lib/rustdesk
export API_SERVER=http://127.0.0.1:21114
/path/to/hbbr &
/path/to/hbbs -r <публичный-хост>
```

Ключи `id_ed25519` / `id_ed25519.pub` создаются при первом старте hbbs в текущем каталоге. Альтернатива флагам — `.env` в cwd (см. `docs/environment-variables.md`).

Проверка: `hbbs --help`, `hbbr --help`.

systemd-юниты-образцы: каталог `systemd/` в репозитории.

---

## Сборка своего Docker-образа

```bash
git clone --recursive https://github.com/temandroid/rustdesk-server.git
cd rustdesk-server
docker build -t ghcr.io/temandroid/rustdesk-server:local .
```

CI (`.github/workflows/publish-rustdesk-server.yml`) на push в `master` публикует amd64 в GHCR.

---

## Обновление

**Важно:** не меняйте volume с ключами (`data/` / `/root` в контейнере). Иначе сменится Key и всем клиентам нужно будет обновлять настройку.

### Docker Compose

```bash
cd /data/rustdesk
docker compose pull
docker compose up -d
# ключи в ./data сохраняются
cat data/id_ed25519.pub   # должен совпасть с прежним
```

Или зафиксируйте тег: `image: ghcr.io/temandroid/rustdesk-server:5e140db` в compose.

### Без Docker

```bash
cd rustdesk-server
git pull --recurse-submodules
cargo build --release
# остановите сервисы, подмените бинарники, запустите снова
# рабочий каталог с id_ed25519* не трогайте
```

### Переезд со стокового OSS rustdesk-server

1. Остановите старые hbbs/hbbr.
2. **Скопируйте** каталог с `id_ed25519`, `id_ed25519.pub` и sqlite peer DB в `data/` нового compose (или оставьте тот же volume).
3. Поднимите `ghcr.io/temandroid/rustdesk-server:latest` с `API_SERVER` на ваш api-srv.
4. Клиентам менять Key не нужно.

Переезд «с Pro» официальный путь описан у RustDesk; для ключей/peer DB принцип тот же — сохранить файлы ключей.

---

## Основные параметры

| Параметр | Flag / env | Кто | Назначение |
|---|---|---|---|
| Key | `-k` / `KEY` | hbbs, hbbr | Ключ шифрования; по умолчанию hbbs грузит/создаёт пару |
| Bind | `-b` / `BIND` | hbbs, hbbr | Локальный адрес прослушивания |
| Port | `-p` / `PORT` | hbbs, hbbr | Базовый порт (hbbs `21116`, hbbr `21117`) |
| Relay | `-r` / `RELAY-SERVERS` | hbbs | Если hbbr на другом хосте/порту |
| **API proxy** | `--api-server` / `API_SERVER` | hbbs | Origin вашего API для `HttpProxyRequest` |
| Force relay | `ALWAYS_USE_RELAY=Y` | hbbs | Только через relay |
| Log | `RUST_LOG` | оба | например `debug` |

Правила API proxy: только пути `/api`; методы GET/POST/PUT/DELETE; удалённый origin — HTTPS (HTTP только loopback); лимиты тела/заголовков; IP клиента в `X-Forwarded-For` / `X-Real-IP`.

Default `API_SERVER`, если не задан: `http://127.0.0.1:<порт_hbbs - 2>` (обычно `21114`).

---

## Лицензия

AGPL-3.0, как у upstream [rustdesk/rustdesk-server](https://github.com/rustdesk/rustdesk-server).
