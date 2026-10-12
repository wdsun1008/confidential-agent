#!/usr/bin/env python3.11
"""Call the native Codex app-server protocol through an authenticated WebSocket."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import time


class WebSocket:
    def __init__(self, host, port, token):
        self.socket = socket.create_connection((host, port), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall((f"GET / HTTP/1.1\r\nHost: {host}:{port}\r\n"
                             "Connection: Upgrade\r\nUpgrade: websocket\r\n"
                             "Sec-WebSocket-Version: 13\r\n"
                             f"Sec-WebSocket-Key: {key}\r\nAuthorization: Bearer {token}\r\n\r\n").encode())
        self.buffer = bytearray()
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise RuntimeError("app-server closed the upgrade connection")
            self.buffer.extend(chunk)
        headers, _, rest = self.buffer.partition(b"\r\n\r\n")
        status = headers.split(b"\r\n", 1)[0].split()[1]
        if status != b"101":
            raise RuntimeError("app-server rejected the WebSocket upgrade: " + status.decode())
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        assert b"sec-websocket-accept: " + accept.lower() in headers.lower()
        self.buffer = bytearray(rest)

    def bytes(self, count):
        while len(self.buffer) < count:
            chunk = self.socket.recv(max(4096, count - len(self.buffer)))
            if not chunk:
                raise RuntimeError("app-server closed before completing the request")
            self.buffer.extend(chunk)
        value = bytes(self.buffer[:count])
        del self.buffer[:count]
        return value

    def send(self, value, opcode=1):
        payload = json.dumps(value).encode() if opcode == 1 else value
        length = len(payload)
        header = bytes([0x80 | opcode])
        if length < 126:
            header += bytes([0x80 | length])
        elif length < 65536:
            header += bytes([0x80 | 126]) + struct.pack("!H", length)
        else:
            header += bytes([0x80 | 127]) + struct.pack("!Q", length)
        mask = os.urandom(4)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        self.socket.sendall(header + mask + masked)

    def receive(self, deadline):
        fragments = bytearray()
        while True:
            self.socket.settimeout(max(0.1, deadline - time.monotonic()))
            first, second = self.bytes(2)
            length = second & 0x7f
            if length == 126:
                length = struct.unpack("!H", self.bytes(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self.bytes(8))[0]
            assert not second & 0x80, "server frames must not be masked"
            assert length < 16 * 1024 * 1024
            payload = self.bytes(length)
            opcode = first & 0x0f
            if opcode == 9:
                self.send(payload, opcode=10)
                continue
            if opcode == 8:
                raise RuntimeError("app-server closed the WebSocket")
            assert opcode in [0, 1], "unexpected WebSocket frame"
            fragments.extend(payload)
            if first & 0x80:
                return json.loads(fragments)

    def request(self, identity, method, params, deadline):
        self.send({"id": identity, "method": method, "params": params})
        while True:
            message = self.receive(deadline)
            if message.get("id") == identity:
                if "error" in message:
                    raise RuntimeError(f"app-server rejected {method}")
                return message["result"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=4500)
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--prompt", required=True)
    parser.add_argument("--expect", required=True)
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()
    token = args.token_file.read_text().strip()
    try:
        rejected = WebSocket(args.host, args.port, "invalid-ca-test-token")
    except RuntimeError as error:
        assert str(error).endswith("401"), "incorrect token did not receive an authentication rejection"
    else:
        rejected.socket.close()
        raise RuntimeError("app-server accepted an incorrect token")
    connection = WebSocket(args.host, args.port, token)
    deadline = time.monotonic() + args.timeout
    try:
        connection.request(1, "initialize", {"clientInfo": {"name": "ca-e2e", "version": "1"},
                           "capabilities": {"experimentalApi": True}}, deadline)
        connection.send({"method": "initialized", "params": {}})
        thread = connection.request(2, "thread/start", {"cwd": "/opt/data/allinone/workspace",
                                    "approvalPolicy": "never", "sandbox": "danger-full-access"}, deadline)
        connection.request(3, "turn/start", {"threadId": thread["thread"]["id"],
                           "input": [{"type": "text", "text": args.prompt}]}, deadline)
        messages = []
        while True:
            message = connection.receive(deadline)
            if message.get("method") == "item/completed":
                item = message["params"]["item"]
                if item["type"] == "agentMessage":
                    messages.append(item["text"])
            if message.get("method") == "turn/completed":
                assert message["params"]["turn"]["status"] == "completed", "model turn failed"
                break
        assert args.expect in "\n".join(messages), "model answer missing expected result"
        print("Codex app-server authentication and native model turn passed")
    finally:
        connection.socket.close()


if __name__ == "__main__":
    main()
