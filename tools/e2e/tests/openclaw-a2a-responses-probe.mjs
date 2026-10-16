import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import http from "node:http";

const marker = "CA_A2A_TEST_OK";
for (const validPeerReply of [true, false]) {
  const requests = [];
  const server = http.createServer(async (request, response) => {
    let text = "";
    for await (const chunk of request) text += chunk;
    const body = JSON.parse(text);
    requests.push([request.url, body]);
    assert.equal(request.headers.authorization, "Bearer test-capability");
    response.setHeader("content-type", "application/json");
    if (requests.length === 1) {
      assert.equal(request.url, "/v1/responses");
      assert.equal(body.tools.length, 1);
      const name = body.tools[0].name;
      // A same-named client tool collides with OpenClaw's native plugin and
      // cannot satisfy its structured client-tool response constraint.
      assert.notEqual(name, "a2a_chat");
      assert.deepEqual(body.tool_choice, { type: "function", name });
      response.end(JSON.stringify({ id: "response-first", output: [{
        type: "function_call", name, call_id: "call-a2a",
        arguments: JSON.stringify({ peer: "peer-beta", message: marker }),
      }] }));
    } else if (requests.length === 2) {
      assert.equal(request.url, "/tools/invoke");
      assert.equal(body.tool, "a2a_chat");
      assert.equal(body.args.peer, "peer-beta");
      assert.equal(body.args.message, marker);
      response.end(JSON.stringify({ ok: true, result: { content: [{
        type: "text", text: JSON.stringify({ text: validPeerReply ? marker : "WRONG_REPLY" }),
      }] } }));
    } else {
      assert.equal(requests.length, 3);
      assert.equal(request.url, "/v1/responses");
      assert.equal(body.previous_response_id, "response-first");
      assert.equal(body.input[0].call_id, "call-a2a");
      assert.equal(JSON.parse(body.input[0].output).text, marker);
      response.end(JSON.stringify({ output: [{ type: "message", content: [{
        type: "output_text", text: marker,
      }] }] }));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const child = spawn(process.execPath, [
      new URL("../probes/openclaw-a2a-responses-probe.mjs", import.meta.url).pathname,
      "--url", `http://127.0.0.1:${server.address().port}`, "--token", "test-capability",
      "--peer", "peer-beta", "--expect", marker, "--timeout-ms", "3000",
    ], { stdio: ["ignore", "pipe", "pipe"], timeout: 10000 });
    let stdout = "", stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    const code = await new Promise((resolve) => child.on("exit", resolve));
    assert.equal(code, validPeerReply ? 0 : 1, stderr);
    assert.equal(requests.length, validPeerReply ? 3 : 2);
    if (validPeerReply) assert.equal(JSON.parse(stdout).finalText, marker);
    else assert.match(stderr, /peer OpenClaw reply does not include expected marker/);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}
console.log("A2A client function remains distinct from native tool and verifies peer output: PASS");
