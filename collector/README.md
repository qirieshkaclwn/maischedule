# mai-collector

Сборщик расписания МАИ. Живёт на машине внутри сети МАИ, раз в сутки скачивает
справочник групп и расписание всех групп, упаковывает в `tar.gz` и отправляет
на основной сервис по `POST /internal/ingest`.

## Зачем

`public.mai.ru` живёт в сети `MAI-CAMPUS` (217.9.88.0/23) и снаружи недоступен:
пакеты умирают на пограничном роутере МАИ. Сборщик решает это тем, что сам
находится внутри сети, а сервер получает готовый снапшот.

## Обход VPN

Если на машине поднят VPN с политической маршрутизацией (Happ/xray, `tun0`,
отдельная таблица маршрутов), все запросы уйдут в туннель. Переменная
`BYPASS_INTERFACE` заставляет сборщик привязать сокеты к физическому
интерфейсу через `SO_BINDTODEVICE` — таблица туннеля для таких сокетов не
срабатывает, маршрут берётся из `main`.

```bash
ip route get 8.8.8.8            # посмотреть, куда уходит трафик
ip -br addr                     # найти имя физического интерфейса
```

Если ядро требует привилегию:

```bash
sudo setcap cap_net_raw+ep ~/mai-collector
```

Ходит мимо VPN только трафик к `public.mai.ru`; отправка снапшота на сервер
идет обычным маршрутом — это отдельный HTTP-клиент без прокси.

## Переменные окружения

| Переменная | По умолчанию | Назначение |
|---|---|---|
| `INGEST_URL` | — | Полный URL приёма, например `https://schedule.example.com/internal/ingest` |
| `INGEST_TOKEN` | — | Тот же токен, что `INGEST_TOKEN` на сервере |
| `BYPASS_INTERFACE` | — | Физический интерфейс, например `wlp2s0` |
| `INTERVAL_HOURS` | `24` | Период успешных отправок |
| `RETRY_MINUTES` | `15` | Пауза после неудачи |
| `CONCURRENCY` | `6` | Параллельных загрузок расписаний |
| `MAI_BASE_URL` | `https://public.mai.ru/schedule/data` | Источник |
| `STATE_PATH` | `collector-state` | Файл с хешем последнего снапшота |
| `CA_FILE` | — | Дополнительный PEM-сертификат (сверх встроенных УЦ Минцифры) |

Встроенные корневые сертификаты Минцифры лежат в `certs/`: без них rustls
не признаёт сертификат `public.mai.ru`.

## Запуск

```bash
# разовая проверка
BYPASS_INTERFACE=wlp2s0 \
INGEST_URL=https://schedule.example.com/internal/ingest \
INGEST_TOKEN=... \
./mai-collector --once

# постоянно, раз в сутки
BYPASS_INTERFACE=wlp2s0 INGEST_URL=... INGEST_TOKEN=... ./mai-collector
```

`--once` полезен для проверки: он делает один цикл и выходит с ошибкой, если
что-то не так.

## systemd

```bash
mkdir -p ~/.config/mai-collector
cp mai-collector ~/.config/mai-collector/
cp .env.example ~/.config/mai-collector/.env   # заполнить INGEST_URL и INGEST_TOKEN
mkdir -p ~/.config/systemd/user
cp deploy/mai-collector.service ~/.config/systemd/user/

systemctl --user daemon-reload
systemctl --user enable --now mai-collector
journalctl --user -u mai-collector -f
```

Сборщик не требует root: он работает от обычного пользователя и не слушает
портов. Ему нужен только исходящий доступ к `public.mai.ru` и к вашему серверу.

## Что происходит при сбое

- Не удалось скачать больше половины расписаний — отправка отменяется, чтобы не
  затереть рабочие данные на сервере. Повтор через `RETRY_MINUTES`.
- Снапшот не изменился (совпал SHA-256) — отправка пропускается.
- Сервер ответил не 2xx — снапшот не считается доставленным, повтор через
  `RETRY_MINUTES`.
