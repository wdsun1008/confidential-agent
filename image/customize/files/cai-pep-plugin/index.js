import net from "node:net";
import crypto from "node:crypto";

const passthroughConfigSchema = {
  safeParse(value) {
    return { success: true, data: value ?? {} };
  },
  uiHints: {
    socketPath: { label: "PEP Socket Path" },
    pepRequired: { label: "Fail Closed When PEP Unavailable" },
    defaultWorkdir: { label: "Default Sandbox Workdir" },
  },
};

function normalizeWorkdir(params, defaultWorkdir) {
  if (!params || typeof params !== "object") {
    return defaultWorkdir;
  }
  if (typeof params.workdir === "string" && params.workdir.trim()) {
    return params.workdir.trim();
  }
  if (typeof params.cwd === "string" && params.cwd.trim()) {
    return params.cwd.trim();
  }
  return defaultWorkdir;
}

function normalizeCommand(params) {
  if (!params || typeof params !== "object") {
    return "";
  }
  if (typeof params.command === "string") {
    return params.command;
  }
  if (typeof params.cmd === "string") {
    return params.cmd;
  }
  return "";
}

function buildRequest({ command, workdir, ctx, socketPath }) {
  return {
    method: "submit_intent",
    id: `req-${crypto.randomUUID()}`,
    params: {
      version: 1,
      run_id: ctx?.runId ?? `run-${Date.now()}`,
      session_key: ctx?.sessionKey ?? "agent:unknown",
      agent_id: ctx?.agentId ?? "main",
      tool_name: "exec",
      skill_id: "cai-pep.exec",
      params: {
        command,
        workdir,
      },
      request_context: {
        provider: "openclaw",
        socket_path: socketPath,
      },
      issued_at_ms: Date.now(),
    },
  };
}

function mapExecResult(result) {
  return {
    stdout: typeof result?.stdout === "string" ? result.stdout : "",
    stderr: typeof result?.stderr === "string" ? result.stderr : "",
    exitCode:
      typeof result?.exit_code === "number"
        ? result.exit_code
        : typeof result?.exitCode === "number"
          ? result.exitCode
          : 1,
    durationMs: typeof result?.duration_ms === "number" ? result.duration_ms : undefined,
    auditId: typeof result?.audit_id === "string" ? result.audit_id : undefined,
    backend: typeof result?.backend === "string" ? result.backend : "docker",
    sandboxProfile:
      typeof result?.sandbox_profile === "string" ? result.sandbox_profile : "default-docker",
  };
}

function submitIntent(socketPath, payload) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ path: socketPath });
    let raw = "";

    socket.on("connect", () => {
      socket.end(JSON.stringify(payload));
    });
    socket.on("data", (chunk) => {
      raw += chunk.toString("utf8");
    });
    socket.on("end", () => {
      try {
        resolve(JSON.parse(raw));
      } catch (error) {
        reject(new Error(`invalid cai-pep response JSON: ${error.message}`));
      }
    });
    socket.on("error", (error) => {
      reject(error);
    });
  });
}

const plugin = {
  id: "cai-pep",
  name: "CAI PEP",
  description: "Redirect exec tool calls to cai-pep over a Unix socket",
  configSchema: passthroughConfigSchema,

  register(api) {
    api.logger.info("cai-pep plugin registered");
    api.on("before_tool_call", async (event, ctx) => {
      if (event?.toolName !== "exec") {
        return {};
      }

      const topLevel = api.config?.confidentialAgent ?? {};
      const pluginConfig = api.pluginConfig ?? {};
      const socketPath =
        pluginConfig.socketPath ??
        topLevel.pepSocket ??
        "/run/cai/pep.sock";
      const pepRequired =
        pluginConfig.pepRequired ?? topLevel.pepRequired ?? true;
      const defaultWorkdir =
        pluginConfig.defaultWorkdir ?? topLevel.defaultWorkdir ?? "/workspace";

      const command = normalizeCommand(event?.params);
      const workdir = normalizeWorkdir(event?.params, defaultWorkdir);
      if (!command.trim()) {
        return {
          block: true,
          blockReason: "cai-pep: exec command is empty",
        };
      }

      const payload = buildRequest({ command, workdir, ctx, socketPath });
      let response;
      try {
        response = await submitIntent(socketPath, payload);
      } catch (error) {
        api.logger.error?.(`cai-pep request failed: ${error.message}`);
        if (!pepRequired) {
          return {};
        }
        return {
          block: true,
          blockReason: `cai-pep unavailable: ${error.message}`,
        };
      }

      if (response?.error) {
        const reason = response.error.audit_id
          ? `${response.error.message} (audit_id=${response.error.audit_id})`
          : response.error.message;
        return {
          block: true,
          blockReason: `cai-pep deny: ${reason}`,
        };
      }

      if (!response?.result) {
        return {
          block: true,
          blockReason: "cai-pep returned no result payload",
        };
      }

      return {
        result: mapExecResult(response.result),
      };
    });
  },
};

export default plugin;
