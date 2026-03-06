# kino

## SG Universe trivia

In *Stargate Universe*, a **kino** is a small Ancient reconnaissance drone.
It can scout ahead (including through a Stargate), record audio/video, and send footage back to the crew on *Destiny*.

## Project description

`kino` is a Rust probe service for ephemeral VM checks in [intar-dev/intar-dev](https://github.com/intar-dev/intar-dev).

- HCL-defined probes, async scraping, in-memory state, protobuf `GET /probes`, no auth.

Implemented probe types:

- `file_exists`
- `file_regex_capture`
- `port_open`
- `k8s_pod_state`
- `command_json_path`

Operational model:

- Designed to run as a systemd service (`kino --config /etc/kino/kino.hcl`)
- Includes Docker-based smoke testing for both the probe service and SSH recording flows
- Smoke Dockerfiles live under `docker/smoke/`
- CI runs `just check` and `just docker-smoke` in one pipeline

Example NetBird probes:

```hcl
probe "netbird_peer_connected" {
  kind = "command_json_path"
  argv = ["netbird", "status", "--json"]
  json_path = "$.peers.connected"
  expected = 1
}

probe "netbird_ssh_enabled" {
  kind = "command_json_path"
  argv = ["netbird", "status", "--json"]
  json_path = "$.sshServer.enabled"
  expected = true
}

probe "netbird_active_ssh_session" {
  kind = "command_json_path"
  argv = ["netbird", "status", "--json"]
  json_path = "$.sshServer.sessions[*].remoteAddress"
}
```

`command_json_path` requires the command to exit successfully, parses stdout as JSON, evaluates the JSONPath, and either checks for any match or an exact JSON value match when `expected` is set.
