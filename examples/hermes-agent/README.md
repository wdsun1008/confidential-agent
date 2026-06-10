# Hermes Agent

This example runs the official Hermes container as a Shelter workload on port
`8642`.

Before using it directly, replace the placeholder `API_SERVER_KEY` and
`DASHSCOPE_API_KEY` values in `hermes-agent.yaml`. The e2e case generates a
fresh API key automatically and injects the DashScope key from the environment.
