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
ssh "${USER}@${SERVER_HOST}" "mkdir -p ${REMOTE_DIR}/data ${REMOTE_DIR}/backups && chmod 777 ${REMOTE_DIR}/data"

echo "2. Сборка Docker-образа под архитектуру linux/amd64..."
docker build --platform linux/amd64 -t maischedule:latest .

echo "3. Экспорт образа в maischedule.tar..."
docker save -o maischedule.tar maischedule:latest

echo "4. Копирование файлов на сервер (сжатие на лету scp -C)..."
scp -C maischedule.tar docker-compose.yml "${USER}@${SERVER_HOST}:${REMOTE_DIR}/"

if ssh "${USER}@${SERVER_HOST}" "[ ! -f ${REMOTE_DIR}/.env ]"; then
    if [ -f ".env" ]; then
        echo "Загрузка локального файла .env на сервер..."
        scp .env "${USER}@${SERVER_HOST}:${REMOTE_DIR}/.env"
    fi
else
    echo "Файл .env уже существует на сервере и сохранен без изменений."
fi

echo "5. Резервное копирование БД, загрузка образа и перезапуск сервиса..."
ssh "${USER}@${SERVER_HOST}" "cd ${REMOTE_DIR} && \
    if [ -f data/maischedule.db ]; then \
        cp data/maischedule.db backups/maischedule_\$(date +%Y%m%d_%H%M%S).db && \
        (ls -t backups/*.db 2>/dev/null | tail -n +6 | xargs -r rm -f --); \
    fi && \
    docker load -i maischedule.tar && \
    docker compose up -d && \
    rm -f maischedule.tar maischedule.tar.gz && \
    docker image prune -f"


echo "6. Очистка локального временного архива..."
rm -f maischedule.tar maischedule.tar.gz

echo "Деплой успешно завершен!"
