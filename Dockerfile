FROM rust:1.93-alpine AS builder
WORKDIR /app

RUN apk add --no-cache build-base musl-dev pkgconfig

COPY Cargo.toml Cargo.lock ./
COPY crates/kino/Cargo.toml crates/kino/Cargo.toml
COPY crates/kino/build.rs crates/kino/build.rs
COPY crates/kino/src crates/kino/src
COPY proto proto

RUN cargo build --release -p kino

FROM k0sproject/k0s:v1.34.3-k0s.0

COPY --from=builder /app/target/release/kino /usr/local/bin/kino

RUN set -eu; \
    chmod +x /usr/local/bin/kino; \
    mkdir -p /etc/kino; \
    { \
      echo 'server {'; \
      echo '  bind = "0.0.0.0"'; \
      echo '  port = 8080'; \
      echo '}'; \
      echo; \
      echo 'defaults {'; \
      echo '  every_seconds = 5'; \
      echo '  timeout_seconds = 2'; \
      echo '  kubeconfig = "/etc/kino/kubeconfig"'; \
      echo '}'; \
      echo; \
      echo 'probe "kino_config_exists" {'; \
      echo '  kind = "file_exists"'; \
      echo '  path = "/etc/kino/kino.hcl"'; \
      echo '}'; \
      echo; \
      echo 'probe "kino_config_has_server_block" {'; \
      echo '  kind = "file_regex_capture"'; \
      echo '  path = "/etc/kino/kino.hcl"'; \
      echo '  pattern = "server\\s*\\{"'; \
      echo '}'; \
      echo; \
      echo 'probe "kube_api_port_open" {'; \
      echo '  kind = "port_open"'; \
      echo '  host = "127.0.0.1"'; \
      echo '  port = 6443'; \
      echo '  protocol = "tcp"'; \
      echo '}'; \
      echo; \
      echo 'probe "kino_check_pod_running" {'; \
      echo '  kind = "k8s_pod_state"'; \
      echo '  namespace = "default"'; \
      echo '  selector = "app=kino-check"'; \
      echo '  desired_state = "phase:Running"'; \
      echo '  every_seconds = 10'; \
      echo '}'; \
    } >/etc/kino/kino.hcl

RUN set -eu; \
    { \
      echo '#!/bin/sh'; \
      echo 'set -eu'; \
      echo; \
      echo 'cleanup() {'; \
      echo '  if [ -n "${KINO_PID:-}" ]; then'; \
      echo '    kill "${KINO_PID}" >/dev/null 2>&1 || true'; \
      echo '  fi'; \
      echo '  if [ -n "${K0S_PID:-}" ]; then'; \
      echo '    kill "${K0S_PID}" >/dev/null 2>&1 || true'; \
      echo '  fi'; \
      echo '}'; \
      echo; \
      echo 'trap cleanup INT TERM EXIT'; \
      echo; \
      echo 'k0s controller --single --ignore-pre-flight-checks &'; \
      echo 'K0S_PID=$!'; \
      echo; \
      echo 'echo "waiting for k0s API readiness..."'; \
      echo 'i=0'; \
      echo 'while [ "${i}" -lt 120 ]; do'; \
      echo '  if k0s kubectl get nodes >/dev/null 2>&1; then'; \
      echo '    break'; \
      echo '  fi'; \
      echo '  i=$((i + 1))'; \
      echo '  sleep 1'; \
      echo 'done'; \
      echo; \
      echo 'if [ "${i}" -ge 120 ]; then'; \
      echo '  echo "k0s did not become ready in time" >&2'; \
      echo '  exit 1'; \
      echo 'fi'; \
      echo; \
      echo 'k0s kubeconfig admin >/etc/kino/kubeconfig'; \
      echo 'echo "waiting for default service account..."'; \
      echo 'j=0'; \
      echo 'while [ "${j}" -lt 120 ]; do'; \
      echo '  if k0s kubectl -n default get serviceaccount default >/dev/null 2>&1; then'; \
      echo '    break'; \
      echo '  fi'; \
      echo '  j=$((j + 1))'; \
      echo '  sleep 1'; \
      echo 'done'; \
      echo 'if [ "${j}" -ge 120 ]; then'; \
      echo '  echo "default service account did not become ready in time" >&2'; \
      echo '  exit 1'; \
      echo 'fi'; \
      echo 'k0s kubectl run kino-check \'; \
      echo '  --image=registry.k8s.io/pause:3.10 \'; \
      echo '  --restart=Never \'; \
      echo '  --labels=app=kino-check \'; \
      echo '  --dry-run=client -o yaml \'; \
      echo '  | k0s kubectl apply -f -'; \
      echo; \
      echo 'kino --config /etc/kino/kino.hcl &'; \
      echo 'KINO_PID=$!'; \
      echo; \
      echo 'wait "${KINO_PID}"'; \
    } >/usr/local/bin/start-kino-k0s.sh

RUN chmod +x /usr/local/bin/start-kino-k0s.sh

EXPOSE 8080 6443

ENTRYPOINT ["/usr/local/bin/start-kino-k0s.sh"]
