#!/usr/bin/env python3.11
"""Check native Codex skill discovery without sending a model request."""
import json
import queue
import subprocess
import threading


def main():
    server = subprocess.Popen(["cai-agent", "app-server", "--listen", "stdio://"],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, text=True, bufsize=1)
    messages = queue.Queue()

    def read():
        for line in server.stdout:
            try:
                messages.put(json.loads(line))
            except ValueError:
                continue
        messages.put(None)

    threading.Thread(target=read, daemon=True).start()

    def send(message):
        server.stdin.write(json.dumps(message) + "\n")
        server.stdin.flush()

    def request(id_, method, params):
        send({"id": id_, "method": method, "params": params})
        while True:
            message = messages.get(timeout=30)
            if message is None:
                raise RuntimeError("Codex app-server closed before answering")
            if message.get("id") == id_:
                if "error" in message:
                    raise RuntimeError("Codex rejected " + method)
                return message["result"]

    try:
        request(1, "initialize", {"clientInfo": {"name": "ca-skill-probe", "version": "1"},
                                  "capabilities": {"experimentalApi": True}})
        send({"method": "initialized", "params": {}})
        result = request(2, "skills/list", {
            "cwds": ["/opt/data/allinone/workspace"], "forceReload": True})
        matches = []
        for entry in result["data"]:
            for skill in entry["skills"]:
                if skill["name"] == "tdx-remote-attestation":
                    assert skill.get("enabled", True), "TDX skill is disabled"
                    matches.append(skill["path"])
        assert any(path.startswith("/opt/data/allinone/home/.agents/skills/")
                   for path in matches), "Codex did not discover its TDX skill"
        print(json.dumps({"tdx_skill_discovered": True, "paths": matches}))
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()


if __name__ == "__main__":
    main()
