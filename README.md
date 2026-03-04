# kino

## SG Universe trivia

In *Stargate Universe*, a **kino** is a small Ancient reconnaissance drone.
It can scout ahead (including through a Stargate), record audio/video, and send footage back to the crew on *Destiny*.

## Project description (from `feat/kino-bootstrap`)

`kino` is a Rust probe service for ephemeral VM checks in [intar-dev/intar-dev](https://github.com/intar-dev/intar-dev).

- HCL-defined probes, async scraping, in-memory state, protobuf `GET /probes`, no auth.

Implemented probe types:

- `file_exists`
- `file_regex_capture`
- `port_open`
- `k8s_pod_state`

Operational model on that branch:

- Designed to run as a systemd service (`kino --config /etc/kino/kino.hcl`)
- Includes Docker-based smoke testing with a single-node k8s runtime
- CI runs `just check` and `just docker-smoke` in one pipeline
