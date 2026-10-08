#!/usr/bin/env python3
"""Run after the JVM is listening. Log in once and store the session cookie."""

import json
import os
import socket

SOCKET = "/tmp/spring.sock"
USER = "atropos"
PASSWORD = "atropos"


def request(method, path, body, cookie=""):
    headers = [
        f"{method} {path} HTTP/1.0",
        "Host: localhost",
        "Connection: close",
        "Content-Type: application/x-www-form-urlencoded",
        f"Content-Length: {len(body)}",
    ]
    if cookie:
        headers.append(f"Cookie: {cookie}")
    payload = ("\r\n".join(headers) + "\r\n\r\n" + body).encode()
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.connect(SOCKET)
    client.sendall(payload)
    chunks = []
    while True:
        chunk = client.recv(65536)
        if not chunk:
            break
        chunks.append(chunk)
    client.close()
    return b"".join(chunks).decode("latin1", "replace")


def session_cookie(response):
    for line in response.split("\r\n"):
        if line.lower().startswith("set-cookie:"):
            pair = line.split(":", 1)[1].strip().split(";", 1)[0]
            if pair.lower().startswith("jsessionid="):
                return pair
    return ""


register = request(
    "POST",
    "/WebGoat/register.mvc",
    f"username={USER}&password={PASSWORD}&matchingPassword={PASSWORD}&agree=agree",
)
cookie = session_cookie(register)
if not cookie:
    login = request("POST", "/WebGoat/login", f"username={USER}&password={PASSWORD}")
    cookie = session_cookie(login)
if not cookie:
    raise SystemExit("WebGoat did not return a session cookie")

os.makedirs("/var/lib/atropos", exist_ok=True)
with open("/var/lib/atropos/auth.json", "w", encoding="utf-8") as handle:
    json.dump({"cookies": {"JSESSIONID": cookie.split("=", 1)[1]}, "fields": {}}, handle)
    handle.write("\n")
