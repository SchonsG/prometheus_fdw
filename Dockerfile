FROM rust:1.86.0-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    libreadline-dev \
    build-essential \
    libxml2-utils \
    libclang-dev \
    libxml2-dev \
    libxslt-dev \
    zlib1g-dev \
    libssl-dev \
    pkg-config \
    libpq-dev \
    xsltproc  \
    ccache \
    bison \
    flex \
    curl \
    git \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install --locked --version 0.12.6 cargo-pgrx \
    && cargo pgrx init --pg15 download

COPY . /prometheus_fdw

WORKDIR /prometheus_fdw

RUN cargo pgrx package

FROM postgres:15

COPY --from=builder /prometheus_fdw/target/release/prometheus_fdw-pg15/usr/lib/postgresql/15/lib/* /usr/lib/postgresql/15/lib/
COPY --from=builder /prometheus_fdw/target/release/prometheus_fdw-pg15/usr/share/postgresql/15/extension/* /usr/share/postgresql/15/extension/

RUN echo "shared_preload_libraries = 'prometheus_fdw'" >> /usr/share/postgresql/postgresql.conf.sample

EXPOSE 5432

CMD ["postgres"]
