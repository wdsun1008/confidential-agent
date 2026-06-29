# Hermes Agent

This example builds a regular mkosi image for Hermes Agent on port `8642`.
During the mkosi post-install step, it clones the official Hermes Agent source
with GitHub fallback and runs the official installer with `--skip-browser`,
`--skip-setup`, and `--non-interactive`. At runtime,
`cai-hermes-agent.service` starts `/usr/local/bin/hermes gateway run` as the
`hermes` user. The AppSpec does not use containers.

Before using it directly, create `secrets/hermes.env` and
`secrets/config.yaml` next to `hermes-agent.yaml`. They are injected as runtime
resources into `/opt/data`; they are not baked into the Shelter build config or
disk image.

`secrets/hermes.env` should contain the API server and DashScope credentials:

```dotenv
API_SERVER_ENABLED=true
API_SERVER_HOST=0.0.0.0
API_SERVER_PORT=8642
API_SERVER_KEY=replace-with-openssl-rand-hex-32
DASHSCOPE_API_KEY=replace-with-dashscope-api-key
DASHSCOPE_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1
HERMES_HOME=/opt/data
HERMES_MODEL=qwen3.7-max
```

`secrets/config.yaml` should select the provider/model:

```yaml
model:
  provider: alibaba
  default: qwen3.7-max
  model: qwen3.7-max
```

`hermes_config` is marked `mutable: true` in the AppSpec because the Hermes
runtime may migrate and rewrite `config.yaml` on startup. The DashScope/API
secret file remains a normal enforced resource.

The root `install-hermes-agent.sh` script selects the Hermes source ref:

```bash
export HERMES_BRANCH='main'
export HERMES_COMMIT=''
```

Set `CA_GITHUB_PROXY_URL` to override the GitHub fallback proxy. The default is
`https://gh-proxy.org/`. Set `HERMES_PYPI_INDEX_URL` or `UV_DEFAULT_INDEX` to
override the Python package index used by the official installer; the default is
`https://mirrors.aliyun.com/pypi/simple/`. Set `HERMES_NPM_REGISTRY_URL` or
`NPM_CONFIG_REGISTRY` to override the npm registry used by optional browser-tool
dependencies; the default is `https://registry.npmmirror.com/`.
