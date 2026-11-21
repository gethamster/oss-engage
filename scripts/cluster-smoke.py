#!/usr/bin/env python3
"""Exercise two already running instances of one disposable PostgreSQL/S3 board."""
import concurrent.futures
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: cluster-smoke.py INSTANCE_A INSTANCE_B")
    a, b = [url.rstrip("/") for url in sys.argv[1:]]
    run = uuid.uuid4().hex[:10]
    channel = f"cluster-{run}"
    token = os.environ.get("ENGAGE_SMOKE_TOKEN")

    def request(url, method, path, body=None, profile=None, raw=None):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        if profile:
            headers["x-board-profile"] = profile
        data = json.dumps(body).encode() if body is not None else None
        if raw is not None:
            data = raw
            headers.update({"Content-Type": "text/plain", "x-board-filename": "proof.txt"})
        req = urllib.request.Request(url + path, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=20) as response:
                content = response.read()
                return response.status, json.loads(content) if "json" in response.headers.get("Content-Type", "") else content
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def ok(result):
        status, body = result
        assert 200 <= status < 300, (status, body)
        return body

    posted = ok(request(a, "POST", "/messages", {"channel": channel, "text": "Cluster proof", "id": run}))
    message = posted["message"]
    mid = message["id"]
    again = ok(request(b, "POST", "/messages", {"channel": channel, "text": "Cluster proof", "id": run}))
    assert again["message"]["id"] == mid, "idempotency is not shared"
    assert ok(request(b, "GET", f"/messages/{mid}"))["message"]["text"] == "Cluster proof"

    def edit(index):
        return request([a, b][index % 2], "POST", f"/messages/{mid}/edit", {"text": f"Edit {index}", "revision": message["revision"]})

    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
        edits = list(executor.map(edit, range(6)))
    assert sum(200 <= status < 300 for status, _ in edits) == 1, "conditional edits were not atomic"
    assert all(status < 300 or (status == 409 and body["error"]["code"] == "revision_conflict") for status, body in edits)

    made = ok(request(a, "POST", "/tasks", {"channel": channel, "text": "One worker at a time"}))
    task_id = made["task"]["id"]

    def claim(index):
        return request([a, b][index % 2], "POST", f"/tasks/{task_id}/claim", {"lease": 30}, profile=f"worker-{index}")

    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
        claims = list(executor.map(claim, range(6)))
    winners = [(index, body) for index, (status, body) in enumerate(claims) if 200 <= status < 300]
    assert len(winners) == 1, "multiple workers acquired a task"
    index, won = winners[0]
    capability = won["claim"]
    public = ok(request(b, "GET", f"/messages/{task_id}"))
    assert capability["token"] not in json.dumps(public), "claim token leaked in public view"
    ok(request(a, "POST", f"/tasks/{task_id}/release", {"epoch": capability["epoch"], "token": capability["token"]}, profile=f"worker-{index}"))
    fresh = ok(request(b, "POST", f"/tasks/{task_id}/claim", {"lease": 30}, profile="replacement"))["claim"]
    stale_status, stale = request(a, "POST", f"/tasks/{task_id}/complete", {**capability, "text": "stale"}, profile=f"worker-{index}")
    assert stale_status == 409 and stale["error"]["code"] == "claim_lost", "stale claim changed the task"
    ok(request(b, "POST", f"/tasks/{task_id}/complete", {"epoch": fresh["epoch"], "token": fresh["token"], "text": "Verified"}, profile="replacement"))

    uploaded = ok(request(a, "POST", "/attachments", raw=b"Shared immutable proof"))
    attachment = uploaded.get("attachment", uploaded)
    ok(request(b, "POST", "/messages", {"channel": channel, "text": "Evidence", "attachments": [attachment["id"]]}))
    assert ok(request(b, "GET", "/attachments/" + attachment["id"])) == b"Shared immutable proof", "blob was not shared"

    snapshot = ok(request(a, "GET", "/threads?channel=" + channel))["cursor"]
    reply = ok(request(b, "POST", "/messages", {"re": mid, "text": "Across instances"}))["message"]
    feed = ok(request(a, "GET", f"/events?channel={channel}&after={snapshot}"))
    assert any(event.get("message", {}).get("id") == reply["id"] for event in feed["events"]), "snapshot/feed gap"
    print(json.dumps({"ok": True, "channel": channel, "checks": ["shared idempotency", "cross-instance read", "conditional edit race", "claim race", "stale fencing", "secret redaction", "shared blob", "snapshot/feed continuity"]}))


if __name__ == "__main__":
    main()
