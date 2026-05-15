#!/usr/bin/env node
import fs from "node:fs";
import http from "node:http";
import path from "node:path";

const listenHost = process.env.CMAAS_LISTEN_HOST || "0.0.0.0";
const listenPort = Number(process.env.CMAAS_LISTEN_PORT || "8000");
const target = new URL(process.env.CMAAS_TARGET || "http://127.0.0.1:8001");
const accessLog = process.env.CMAAS_ACCESS_LOG || "/var/log/cmaas-access.log";

fs.mkdirSync(path.dirname(accessLog), { recursive: true });

function appendAccess(req, status, error) {
  const remote = req.socket.remoteAddress || "-";
  const line = [
    new Date().toISOString(),
    remote,
    req.method || "-",
    req.url || "-",
    String(status),
    error ? String(error).replace(/\s+/g, " ").slice(0, 240) : "-",
  ].join("\t");
  fs.appendFile(accessLog, `${line}\n`, () => {});
}

function proxyHeaders(req) {
  const headers = { ...req.headers };
  for (const name of [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
  ]) {
    delete headers[name];
  }
  headers.host = target.host;
  return headers;
}

const server = http.createServer((req, res) => {
  const upstream = http.request(
    {
      protocol: target.protocol,
      hostname: target.hostname,
      port: target.port,
      method: req.method,
      path: req.url,
      headers: proxyHeaders(req),
    },
    (upstreamRes) => {
      res.writeHead(upstreamRes.statusCode || 502, upstreamRes.headers);
      upstreamRes.pipe(res);
      upstreamRes.on("end", () => appendAccess(req, upstreamRes.statusCode || 0));
    },
  );

  upstream.on("error", (error) => {
    if (!res.headersSent) {
      res.writeHead(502, { "content-type": "text/plain; charset=utf-8" });
    }
    res.end("upstream unavailable\n");
    appendAccess(req, 502, error.message);
  });

  req.pipe(upstream);
});

server.on("clientError", (error, socket) => {
  socket.end("HTTP/1.1 400 Bad Request\r\n\r\n");
});

server.listen(listenPort, listenHost, () => {
  console.log(`cmaas access proxy listening on ${listenHost}:${listenPort}, target=${target.origin}`);
});
