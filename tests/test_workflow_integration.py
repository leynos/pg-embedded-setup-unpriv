from __future__ import annotations

import json
import subprocess
from pathlib import Path

EVENT = Path("tests/fixtures/pull_request.event.json")


def run_act(
    job: str = "selftest",
    event_path: Path = EVENT,
    *,
    artifact_dir: Path,
) -> tuple[int, Path, str]:
    artifact_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        "act",
        "pull_request",
        "-j",
        job,
        "-e",
        str(event_path),
        "-P",
        "ubuntu-latest=catthehacker/ubuntu:act-latest",
        "--artifact-server-path",
        str(artifact_dir),
        "--json",
        "-b",
    ]
    completed = subprocess.run(cmd, text=True, capture_output=True, check=False)
    logs = f"{completed.stdout}\n{completed.stderr}"
    return completed.returncode, artifact_dir, logs


def test_workflow_produces_expected_artefact_and_logs(tmp_path: Path) -> None:
    artifact_dir = tmp_path / "act-artifacts"
    code, artdir, logs = run_act(artifact_dir=artifact_dir)
    assert code == 0, f"act failed:\n{logs}"

    files = list(artdir.rglob("result*/result.json"))
    assert files, f"artefact missing. Logs:\n{logs}"
    data = json.loads(files[0].read_text())
    assert data["status"] == "ok"
    assert data["python"].startswith("3."), data["python"]
    assert data["env"]["GITHUB_WORKFLOW"] == "workflow-selftest"

    saw_greeting = False
    for line in logs.splitlines():
        if not line.lstrip().startswith("{"):
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        output = evt.get("Output") or evt.get("message") or ""
        if "Hello from workflow" in output:
            saw_greeting = True
            break
    assert saw_greeting, "expected greeting in structured logs"
