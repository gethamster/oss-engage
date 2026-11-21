#!/usr/bin/env python3
"""Qualify disposable SQLite and two PostgreSQL replicas, including restart recovery."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".cache/ci-protocol"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/engage")
    parser.add_argument("--prepare", action="store_true")
    args = parser.parse_args()
    database = os.environ.get("ENGAGE_TEST_POSTGRES")
    if not database:
        parser.error("ENGAGE_TEST_POSTGRES must name a disposable PostgreSQL database")
    CACHE.mkdir(parents=True, exist_ok=True)
    processes = {}
    logs = []
    ports = {"sqlite": 56321, "postgres-a": 56322, "postgres-b": 56323}
    base_env = dict(os.environ)
    # A qualification board is local and must not inherit a real user's credentials.
    for key in list(base_env):
        if key.startswith(("ENGAGE_OIDC_", "ENGAGE_SERVER_TOKEN", "ENGAGE_DATABASE_URL", "ENGAGE_SYNC_CONFIG", "ENGAGE_PUBLIC_URL", "BOARD_TOKEN", "BOARD_PROFILE", "BOARD_NAME")):
            base_env.pop(key, None)

    def api(name, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"http://127.0.0.1:{ports[name]}{path}", data=data, headers={"Content-Type": "application/json"}, method=method)
        with urllib.request.urlopen(req, timeout=20) as response:
            return json.load(response)

    def start(name):
        env = dict(base_env)
        if name.startswith("postgres"):
            env["ENGAGE_DATABASE_URL"] = database
            for source, target in [("ENGAGE_TEST_S3_ENDPOINT", "ENGAGE_S3_ENDPOINT"), ("ENGAGE_TEST_S3_BUCKET", "ENGAGE_S3_BUCKET")]:
                if source in env:
                    env[target] = env[source]
        else:
            env.pop("ENGAGE_S3_ENDPOINT", None)
            env.pop("ENGAGE_S3_BUCKET", None)
        log = (CACHE / f"{name}.log").open("a")
        logs.append(log)
        state = CACHE / ("shared" if name.startswith("postgres") else "sqlite")
        processes[name] = subprocess.Popen([str(args.binary.resolve()), "serve", "--port", str(ports[name]), "--dir", str(state), "--no-inspect"], env=env, cwd=ROOT, stdout=log, stderr=log)
        for _ in range(100):
            if processes[name].poll() is not None:
                raise RuntimeError(f"{name} exited; inspect its task-owned log")
            try:
                if api(name, "GET", "/readyz")["status"] == "ready":
                    return
            except (OSError, urllib.error.HTTPError):
                pass
            time.sleep(0.1)
        raise RuntimeError(f"{name} did not become ready")

    def stop(name):
        process = processes.pop(name)
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)

    try:
        for name in ports:
            start(name)
        for name in ["sqlite", "postgres-a"]:
            command = ["python3", "scripts/conformance.py", "--url", f"http://127.0.0.1:{ports[name]}", "--report", str(CACHE / f"{name}.json")]
            if args.prepare:
                command.append("--prepare")
            subprocess.run(command, cwd=ROOT, env=base_env, check=True)
        subprocess.run(["python3", "scripts/cluster-smoke.py", f"http://127.0.0.1:{ports['postgres-a']}", f"http://127.0.0.1:{ports['postgres-b']}"], cwd=ROOT, env=base_env, check=True)
        channel = "recovery-" + uuid.uuid4().hex[:10]
        api("postgres-a", "POST", "/watchers", {"watch": {"recovery": {"tail": channel}}})
        message = api("postgres-b", "POST", "/messages", {"channel": channel, "text": "Survive a restart"})["message"]
        matches = api("postgres-a", "GET", "/watchers/matches")["matches"]
        leased = next(match for match in matches if match["event"].get("message", {}).get("id") == message["id"])
        stop("postgres-a")
        assert not any(match["id"] == leased["id"] for match in api("postgres-b", "GET", "/watchers/matches")["matches"]), "another replica stole an unexpired lease"
        api("postgres-b", "POST", f"/watchers/matches/{leased['id']}/ack", {"lease": leased["lease"]})
        start("postgres-a")
        assert not any(match["id"] == leased["id"] for match in api("postgres-a", "GET", "/watchers/matches")["matches"]), "acknowledged delivery replayed after restart"
        api("postgres-a", "POST", "/watchers", {"watch": {}})
        scheduled = api("sqlite", "POST", "/schedules", {"channel": channel, "text": "Durable schedule", "in": "1s"})["schedule"]
        stop("sqlite")
        time.sleep(1.1)
        start("sqlite")
        delivered = []
        for _ in range(50):
            delivered = api("sqlite", "GET", "/messages?channel=" + channel)["messages"]
            if delivered:
                break
            time.sleep(0.1)
        assert len(delivered) == 1 and delivered[0]["schedule"]["id"] == scheduled["id"], "scheduled send did not recover exactly once"
        report = {"ok": True, "checks": ["SQLite conformance", "PostgreSQL conformance", "two-replica shared storage and claims", "unexpired delivery lease survives owner restart", "cross-replica acknowledgement survives restart", "one-time SQLite schedule survives downtime"]}
        (CACHE / "recovery.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))
    finally:
        for name in list(processes):
            stop(name)
        for log in logs:
            log.close()


if __name__ == "__main__":
    main()
