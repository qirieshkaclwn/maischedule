#!/usr/bin/env bash
set -e

SERVER_HOST="${1}"
USER="${2:-root}"
REMOTE_DIR="${3:-~/maischedule}"

if [ -z "$SERVER_HOST" ]; then
    echo "Использование: ./scripts/deploy.sh <IP_ИЛИ_ХОСТ_СЕРВЕРА> [USER] [REMOTE_DIR]"
    exit 1
fi

echo "--- Запуск деплоя на ${USER}@${SERVER_HOST} (${REMOTE_DIR}) ---"

echo "1. Подготовка рабочей директории на сервере..."
ssh "${USER}@${SERVER_HOST}" "mkdir -p ${REMOTE_DIR}/data ${REMOTE_DIR}/backups ${REMOTE_DIR}/nginx/conf.d ${REMOTE_DIR}/data/certbot/conf ${REMOTE_DIR}/data/certbot/www ${REMOTE_DIR}/data/certbot/logs && chmod 777 ${REMOTE_DIR}/data"

echo "2. Сборка Docker-образа под архитектуру linux/amd64..."
docker build --platform linux/amd64 -t maischedule:latest .

echo "3. Экспорт образа в maischedule.tar..."
docker save -o maischedule.tar maischedule:latest

echo "4. Копирование файлов на сервер (сжатие на лету scp -C)..."
scp -C maischedule.tar docker-compose.yml scripts/server_deploy.sh "${USER}@${SERVER_HOST}:${REMOTE_DIR}/"
scp -r -C nginx "${USER}@${SERVER_HOST}:${REMOTE_DIR}/"

if [ -f ".env" ]; then
    echo "Проверка конфигурации .env на сервере..."
    REMOTE_HASH=$(ssh "${USER}@${SERVER_HOST}" "if [ -f '${REMOTE_DIR}/.env' ]; then sha256sum '${REMOTE_DIR}/.env' | cut -d ' ' -f 1; else echo missing; fi" | tr -d '\r\n ')
    if [ "$REMOTE_HASH" = "missing" ]; then
        echo "Файл .env отсутствует на сервере. Загрузка локального .env..."
        scp .env "${USER}@${SERVER_HOST}:${REMOTE_DIR}/.env"
    else
        if command -v sha256sum >/dev/null 2>&1; then
            LOCAL_HASH=$(sha256sum .env | cut -d ' ' -f 1 | tr -d '\r\n ')
        else
            LOCAL_HASH=$(shasum -a 256 .env | cut -d ' ' -f 1 | tr -d '\r\n ')
        fi

        if [ "$LOCAL_HASH" != "$REMOTE_HASH" ]; then
            echo "Локальный .env изменился. Создание резервной копии и обновление на сервере..."
            BACKUP_TIME=$(date +%Y%m%d_%H%M%S)
            ssh "${USER}@${SERVER_HOST}" "cp '${REMOTE_DIR}/.env' '${REMOTE_DIR}/backups/.env_${BACKUP_TIME}.bak'"
            scp .env "${USER}@${SERVER_HOST}:${REMOTE_DIR}/.env"
            echo "Файл .env успешно обновлен."
        else
            echo "Файл .env на сервере уже актуален."
        fi
    fi
else
    echo "Локальный файл .env не найден."
fi

echo "5. Выполнение развертывания на сервере (проверка SSL Let's Encrypt, бэкап, перезапуск)..."
ssh "${USER}@${SERVER_HOST}" "chmod +x ${REMOTE_DIR}/server_deploy.sh && ${REMOTE_DIR}/server_deploy.sh ${REMOTE_DIR}"


echo "6. Очистка локального временного архива..."
rm -f maischedule.tar maischedule.tar.gz

echo "Деплой успешно завершен!"
