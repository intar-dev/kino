import json
import pathlib
import sys


def main() -> None:
    recordings_dir = pathlib.Path(sys.argv[1])
    files = sorted(recordings_dir.glob("*.cast"))
    if len(files) < 2:
        raise SystemExit(f"expected at least 2 cast files, found {len(files)}")

    all_payloads: list[str] = []
    for path in files:
        lines = path.read_text(encoding="utf-8").splitlines()
        if len(lines) < 2:
            raise SystemExit(f"{path.name} is missing event lines")

        header = json.loads(lines[0])
        if header.get("version") != 2:
            raise SystemExit(f"{path.name} has wrong version header: {header!r}")
        if not isinstance(header.get("width"), int) or not isinstance(
            header.get("height"), int
        ):
            raise SystemExit(f"{path.name} has invalid dimensions: {header!r}")
        if "env" not in header or "SHELL" not in header["env"]:
            raise SystemExit(f"{path.name} is missing env.SHELL: {header!r}")

        for raw in lines[1:]:
            event = json.loads(raw)
            if not (isinstance(event, list) and len(event) == 3):
                raise SystemExit(f"{path.name} has malformed event: {event!r}")
            if not isinstance(event[0], (int, float)):
                raise SystemExit(f"{path.name} has non-numeric timestamp: {event!r}")
            if event[1] not in {"i", "o", "r", "m"}:
                raise SystemExit(f"{path.name} has invalid event type: {event!r}")
            if not isinstance(event[2], str):
                raise SystemExit(f"{path.name} has non-string payload: {event!r}")
            all_payloads.append(event[2])

    joined = "\n".join(all_payloads)
    if "interactive-smoke" not in joined:
        raise SystemExit("interactive session payload missing from cast files")
    if "command-smoke" not in joined:
        raise SystemExit("command session payload missing from cast files")


if __name__ == "__main__":
    main()
