#!/usr/bin/env bash
set -e

REMOTE_DIR="${1:-$(pwd)}"
cd "$REMOTE_DIR"

echo "=== Запуск развертывания на сервере ($REMOTE_DIR) ==="

# 1. Чтение домена и email из .env
DOMAIN="schedule.example.com"
LETSENCRYPT_EMAIL=""

if [ -f .env ]; then
    ENV_DOMAIN=$(grep -E '^[[:space:]]*DOMAIN=' .env | cut -d '=' -f2- | tr -d '\r' | tr -d '"' | tr -d "'" | xargs)
    if [ -n "$ENV_DOMAIN" ]; then
        DOMAIN="$ENV_DOMAIN"
    fi
    ENV_EMAIL=$(grep -E '^[[:space:]]*LETSENCRYPT_EMAIL=' .env | cut -d '=' -f2- | tr -d '\r' | tr -d '"' | tr -d "'" | xargs)
    if [ -n "$ENV_EMAIL" ]; then
        LETSENCRYPT_EMAIL="$ENV_EMAIL"
    fi
fi

echo "Целевой домен: $DOMAIN"

# 2. Подготовка каталогов
mkdir -p data backups nginx/conf.d data/certbot/conf data/certbot/www data/certbot/logs
chmod 777 data

# 3. Актуализация настроек в .env
if [ -f .env ]; then
    if ! grep -qE '^[[:space:]]*DOMAIN=' .env; then
        echo "Добавление DOMAIN=$DOMAIN в .env..."
        echo "DOMAIN=$DOMAIN" >> .env
    fi

    # Если BASE_URL еще настроен на http IP или localhost, переключаем на https домен
    if grep -qE '^[[:space:]]*BASE_URL=http://' .env; then
        echo "Обновление BASE_URL в .env на https://${DOMAIN}..."
        sed -i "s|^[[:space:]]*BASE_URL=.*|BASE_URL=https://${DOMAIN}|" .env
    fi
fi

# 4. Генерация конфигурации Nginx
if [ -f nginx/default.conf.template ]; then
    echo "Генерация nginx/conf.d/default.conf для домена $DOMAIN..."
    sed "s|\${DOMAIN}|${DOMAIN}|g" nginx/default.conf.template > nginx/conf.d/default.conf
fi

# 5. Проверка и выпуск SSL-сертификата Let's Encrypt
CERT_FILE="data/certbot/conf/live/${DOMAIN}/fullchain.pem"
if [ ! -f "$CERT_FILE" ]; then
    echo "SSL-сертификат для $DOMAIN не найден. Запрос сертификата Let's Encrypt..."
    # Останавливаем nginx, чтобы освободить порт 80 для certbot standalone
    docker compose stop nginx 2>/dev/null || true

    EMAIL_ARG="--register-unsafely-without-email"
    if [ -n "$LETSENCRYPT_EMAIL" ]; then
        EMAIL_ARG="--email $LETSENCRYPT_EMAIL"
    fi

    set +e
    docker run --rm \
        -p 80:80 \
        -v "$(pwd)/data/certbot/conf:/etc/letsencrypt" \
        -v "$(pwd)/data/certbot/www:/var/www/certbot" \
        -v "$(pwd)/data/certbot/logs:/var/log/letsencrypt" \
        certbot/certbot certonly --standalone \
        -d "$DOMAIN" \
        --agree-tos \
        $EMAIL_ARG \
        --non-interactive
    CERT_RESULT=$?
    set -e

    if [ $CERT_RESULT -eq 0 ] && [ -f "$CERT_FILE" ]; then
        echo "Сертификат Let's Encrypt для $DOMAIN успешно получен!"
    else
        echo "Предупреждение: Не удалось выпустить сертификат Let's Encrypt."
        echo "Убедитесь, что DNS A-запись для $DOMAIN указывает на публичный IP этого сервера."
        echo "Создание временного самоподписанного сертификата для корректного запуска Nginx..."
        mkdir -p "data/certbot/conf/live/${DOMAIN}"
        openssl req -x509 -nodes -newkey rsa:2048 -days 30 \
            -keyout "data/certbot/conf/live/${DOMAIN}/privkey.pem" \
            -out "data/certbot/conf/live/${DOMAIN}/fullchain.pem" \
            -subj "/CN=${DOMAIN}"
    fi
else
    echo "SSL-сертификат для $DOMAIN уже существует: $CERT_FILE"
fi

# 6. Резервное копирование SQLite БД
if [ -f data/maischedule.db ]; then
    BACKUP_NAME="backups/maischedule_$(date +%Y%m%d_%H%M%S).db"
    echo "Создание бэкапа базы данных: $BACKUP_NAME"
    cp data/maischedule.db "$BACKUP_NAME"
    (ls -t backups/*.db 2>/dev/null | tail -n +6 | xargs -r rm -f --) || true
fi

# 7. Загрузка образа и запуск контейнеров
if [ -f maischedule.tar ]; then
    echo "Загрузка обновленного Docker-образа..."
    docker load -i maischedule.tar
    rm -f maischedule.tar maischedule.tar.gz
fi

echo "Запуск сервисов через docker compose..."
docker compose up -d --remove-orphans

echo "Очистка устаревших образов..."
docker image prune -f

# 8. Настройка автоматического продления сертификатов раз в неделю через cron
if command -v crontab >/dev/null 2>&1; then
    RENEW_CMD="0 3 * * 1 cd $REMOTE_DIR && docker run --rm -v \$(pwd)/data/certbot/conf:/etc/letsencrypt -v \$(pwd)/data/certbot/www:/var/www/certbot certbot/certbot renew --webroot -w /var/www/certbot --quiet && docker compose restart nginx >/dev/null 2>&1"
    (crontab -l 2>/dev/null | grep -v "certbot renew" || true; echo "$RENEW_CMD") | crontab - 2>/dev/null || true
fi

echo "=== Развертывание успешно завершено! ==="
