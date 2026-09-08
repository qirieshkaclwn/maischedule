#!/usr/bin/env bash
set -e

REMOTE_DIR="${1:-$(pwd)}"
cd "$REMOTE_DIR"

echo "Проверка и продление SSL-сертификатов Let's Encrypt..."
docker run --rm \
    -v "$(pwd)/data/certbot/conf:/etc/letsencrypt" \
    -v "$(pwd)/data/certbot/www:/var/www/certbot" \
    -v "$(pwd)/data/certbot/logs:/var/log/letsencrypt" \
    certbot/certbot renew --webroot -w /var/www/certbot

echo "Перезагрузка Nginx для применения обновленных сертификатов..."
docker compose exec nginx nginx -s reload 2>/dev/null || docker compose restart nginx
echo "Сертификаты успешно проверены."
