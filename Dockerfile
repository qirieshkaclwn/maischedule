# ========================================================
# Stage 1: Сборка статического Rust бинарника (Builder)
# ========================================================
FROM rust:alpine AS builder

WORKDIR /app

# Устанавливаем необходимые зависимости для сборки C/C++ (для rusqlite bundled)
RUN apk add --no-cache musl-dev gcc make

# Кэширование сборки зависимостей: сначала копируем только Cargo.toml
COPY Cargo.toml .

# Создаем пустышку для сборки внешних библиотек
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Копируем реальный исходный код и шаблоны
COPY src/ ./src/
COPY templates/ ./templates/

# Обновляем timestamp исходников, чтобы cargo пересобрал бинарник
RUN touch src/main.rs && cargo build --release

# ========================================================
# Stage 2: Финальный ультра-легковесный образ (Runner)
# Размер всего ~18 МБ, потребление RAM ~5-10 МБ!
# ========================================================
FROM alpine:3.20 AS runner

# ca-certificates нужны для HTTPS запросов к public.mai.ru и api.telegram.org
# tzdata нужен для корректной работы таймзоны Europe/Moscow
RUN apk add --no-cache ca-certificates tzdata

WORKDIR /app

# Копируем скомпилированный бинарник
COPY --from=builder /app/target/release/maischedule /usr/local/bin/maischedule

# Создаем непривилегированного пользователя и каталог для SQLite базы данных
RUN addgroup -S appgroup && adduser -S appuser -G appgroup && \
    mkdir -p /app/data && chmod 777 /app/data && chown -R appuser:appgroup /app

# Объявляем каталог базы данных томом для сохранения данных между перезапусками
VOLUME ["/app/data"]

USER appuser


EXPOSE 8000

# Встроенный healthcheck на базе wget (входит в стандартный busybox alpine)
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8000/health || exit 1

CMD ["/usr/local/bin/maischedule"]
