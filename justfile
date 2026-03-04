check:
        cargo fmt --all -- --check
        cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic
        cargo nextest run --workspace

docker-smoke:
        #!/usr/bin/env bash
        set -euo pipefail

        container_name="kino-k0s-ci"
        image_tag="kino-k0s:ci"

        cleanup() {
          docker rm -f "${container_name}" >/dev/null 2>&1 || true
        }

        wait_for_probes() {
          local attempts=0
          local max_attempts=180
          local http_code
          local state

          echo "Waiting for /probes readiness..."
          while ((attempts < max_attempts)); do
            state="$(docker inspect -f '{{{{.State.Status}}}}' "${container_name}" 2>/dev/null || echo unknown)"
            if [[ "${state}" == "exited" || "${state}" == "dead" ]]; then
              echo "Container is ${state} before /probes became ready"
              docker logs --tail 200 "${container_name}" || true
              return 1
            fi

            http_code="$(curl -sS -o /tmp/kino-probes.bin -w '%{http_code}' --max-time 2 http://127.0.0.1:18080/probes || true)"
            if [[ "${http_code}" == "200" ]]; then
              echo "/probes is ready"
              return 0
            fi

            ((attempts += 1))
            if ((attempts % 10 == 0)); then
              echo "Still waiting (attempt ${attempts}/${max_attempts}, http_code=${http_code})"
            fi
            sleep 1
          done

          echo "Timed out waiting for /probes"
          docker logs --tail 200 "${container_name}" || true
          return 1
        }

        assert_probe_status() {
          local probe_id="$1"
          local expected_status="$2"

          if ! grep -F -A8 "id: \"${probe_id}\"" /tmp/kino-probes.txt | grep -Fq "status: ${expected_status}"; then
            echo "Probe ${probe_id} did not report ${expected_status}"
            cat /tmp/kino-probes.txt
            return 1
          fi
        }

        trap cleanup EXIT
        cleanup

        docker build -t "${image_tag}" .
        docker run -d --name "${container_name}" --privileged -p 18080:8080 -p 16443:6443 "${image_tag}" >/dev/null

        wait_for_probes

        protoc --decode=kino.v1.ProbesSnapshotV1 -I proto proto/kino/v1/probes.proto < /tmp/kino-probes.bin >/tmp/kino-probes.txt
        cat /tmp/kino-probes.txt

        for probe_id in kino_check_pod_running kino_config_exists kino_config_has_server_block kube_api_port_open; do
          if ! grep -Fq "id: \"${probe_id}\"" /tmp/kino-probes.txt; then
            echo "Missing probe id: ${probe_id}"
            exit 1
          fi
        done

        assert_probe_status "kino_config_exists" "PROBE_STATUS_PASS"
        assert_probe_status "kino_config_has_server_block" "PROBE_STATUS_PASS"
        assert_probe_status "kube_api_port_open" "PROBE_STATUS_PASS"
