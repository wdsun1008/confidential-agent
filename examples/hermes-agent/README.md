# Hermes Agent

This example runs the official Hermes container as a Shelter `podman` runtime
workload on port `8642`.

Before using it directly, create `secrets/hermes.env` and
`secrets/config.yaml` next to `hermes-agent.yaml`. They are injected as runtime
resources into the guest at `/var/lib/confidential-agent/hermes-agent/data` and
then bind-mounted into the container as `/opt/data`; they are not baked into the
Shelter build config or image.

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
container migrates and rewrites `config.yaml` on startup. The DashScope/API
secret file remains a normal enforced resource.

The container still starts through the image's normal `/init` entrypoint. The
AppSpec only passes `gateway run` as the container command and uses Shelter
runtime mounts to bridge Confidential Agent resource injection into the
container.
